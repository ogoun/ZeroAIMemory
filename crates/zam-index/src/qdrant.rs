//! Адаптер [`VectorIndex`] к Qdrant 1.19 по gRPC (`qdrant-client`, исследование 05 §5.2).
//!
//! Коллекция `zam_chunks` (§6.4):
//! - `dense` — Cosine, HNSW m=16, оригиналы `Memory::Cold` (mmap; `Pinned` для оригиналов
//!   в 1.19 не поддерживается), int8 scalar quantization quantile 0.99 `Memory::Pinned`,
//!   поиск по int8 с rescoring по оригиналам; тип оригиналов — f32 по умолчанию
//!   (f16 — гипотеза исследования, проверяется на eval);
//! - `sparse` — sparse-индекс `Memory::Cached`, без IDF-модификатора;
//! - `on_disk_payload`, 2 сегмента, метаданные `{model_id, schema}`;
//! - payload-индексы: `user_id` (keyword, `is_tenant`), `space_id`, `node_id`,
//!   `path_ancestors`, `level` (keyword), `updated_at` (integer); с M3 — `entry_id`, `kind`,
//!   `status`, `insight_kind`, `certainty`, `area` (keyword). Коллекцию M2 `ensure` дополняет
//!   недостающими индексами на месте, `schema` остаётся 1;
//! - фильтр — дизъюнкция областей `(user_id, space_id ∈ …)` (`should`) и общие условия
//!   (`must`, статусы — `must_not`), один для prefetch чанков и сводок;
//! - с `VectorFilter::folders` (M3, канал папок §8.3) — ещё prefetch dense top-10 по точкам
//!   папок со своим фильтром ([`VectorFilter::folder_filter`]); итоговый лимит растёт на
//!   его размер, так что точки папок не вытесняют чанки из выдачи M2.
//!
//! Задача 19 M3 — первый старт после жёсткой остановки ([`CollectionTuning`]): клиент
//! супервизора создаёт коллекцию с заданными `flush_interval_sec` и `wal_capacity_mb`, у
//! существующей обновляет `flush_interval_sec` (размер блока WAL Qdrant 1.19 у существующей
//! коллекции не меняет: в `UpdateCollection` нет `wal_config`) и отмечает время каждой
//! записи в [`WriteMark`] — по нему супервизор ждёт сброса WAL перед остановкой.
//!
//! **Точный поиск** ([`QdrantIndex::with_exact_search`], `qdrant.exact_search`, задача 15
//! M4): `SearchParams.exact = true` и `quantization.ignore = true` — полный перебор по
//! оригиналам вместо HNSW и int8. Только для прогонов гейта качества `cargo xtask eval`
//! (решение владельца 2026-10-26, §18.2): порог гейта мерит квантование модели, а не шум
//! HNSW-индекса. В продакшне выключен.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use qdrant_client::qdrant::r#match::MatchValue;
use qdrant_client::qdrant::value::Kind;
use qdrant_client::qdrant::vectors_config::Config as VectorsConfigKind;
use qdrant_client::qdrant::with_payload_selector::SelectorOptions;
use qdrant_client::qdrant::{
    Condition, CountPointsBuilder, CreateCollectionBuilder, CreateFieldIndexCollectionBuilder,
    DeleteCollectionBuilder, DeletePointsBuilder, Distance, FieldType, Filter,
    HnswConfigDiffBuilder, KeywordIndexParamsBuilder, Memory, NamedVectors,
    OptimizersConfigDiffBuilder, PayloadIncludeSelector, PointStruct, PrefetchQueryBuilder,
    QuantizationSearchParamsBuilder, Query, QueryPointsBuilder, RrfBuilder,
    ScalarQuantizationBuilder, ScoredPoint, ScrollPointsBuilder, SearchParamsBuilder,
    SparseIndexConfigBuilder, SparseVectorParamsBuilder, SparseVectorsConfigBuilder,
    UpdateCollectionBuilder, UpsertPointsBuilder, Value, Vector, VectorInput, VectorParamsBuilder,
    VectorsConfigBuilder, WalConfigDiffBuilder,
};
use qdrant_client::{Payload, Qdrant, QdrantError};
use tonic::Code;
use zam_core::{Id, TargetRef};

use crate::IndexError;
use crate::vector::{
    HIT_FIELDS, IndexFuture, IndexMeta, IndexPoint, LEVEL_FOLDER, VectorFilter, VectorHit,
    VectorIndex, VectorScope, check_points, hit_target, point_id, split_targets,
};

/// Имя коллекции (§6.4).
pub const COLLECTION: &str = "zam_chunks";
/// Имя dense-вектора.
pub const DENSE: &str = "dense";
/// Имя sparse-вектора.
pub const SPARSE: &str = "sparse";
/// Версия схемы коллекции в метаданных (`schema`).
pub const SCHEMA_VERSION: i64 = 1;

/// Число сегментов коллекции. По умолчанию Qdrant берёт число ядер (8–24), и каждая
/// операция над схемой (payload-индекс) сбрасывает на диск каждый сегмент: на HDD это
/// 3–4 с на индекс. Поиск у нас идёт в 2 потока (`max_search_threads: 2`, исследование 05
/// §6.2), поэтому больше двух сегментов параллелизма не добавляют.
const SEGMENTS: u64 = 2;
/// Размер пачки upsert.
const UPSERT_BATCH: usize = 256;
/// Prefetch dense и sparse по чанкам.
const LEAF_PREFETCH: u64 = 50;
/// Prefetch dense по заметкам и секциям.
const NOTE_PREFETCH: u64 = 10;
/// Prefetch dense по папкам (канал папок).
const FOLDER_PREFETCH: u64 = 10;
/// Параметр k у RRF.
const RRF_K: u32 = 60;
/// Итоговый лимит слияния.
const QUERY_LIMIT: u64 = 60;
/// Предел длины сообщения сервера в ошибке.
const MAX_ERR_MSG: usize = 200;
/// Предел установки соединения. Qdrant — на localhost (§4.3), соединение занимает доли
/// миллисекунды; на Windows отказ соединения с закрытым портом localhost приходит лишь
/// через ~2 с (повторы SYN), а клиент повторяет запрос после ошибки транспорта — упавший
/// Qdrant задерживал поиск на ~4 с (замер задачи 15 M2). С пределом — не дольше двух
/// попыток по 300 мс.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(300);

const LEVEL_CHUNK: &str = "chunk";
const LEVELS_NOTE: [&str; 2] = ["note", "section"];

/// Payload-индексы коллекции: (поле, тип, тенант). Последние шесть добавлены в M3:
/// `ensure` досоздаёт их в существующей коллекции.
const PAYLOAD_INDEXES: [(&str, FieldType, bool); 12] = [
    ("user_id", FieldType::Keyword, true),
    ("space_id", FieldType::Keyword, false),
    ("node_id", FieldType::Keyword, false),
    ("path_ancestors", FieldType::Keyword, false),
    ("level", FieldType::Keyword, false),
    ("updated_at", FieldType::Integer, false),
    ("entry_id", FieldType::Keyword, false),
    ("kind", FieldType::Keyword, false),
    ("status", FieldType::Keyword, false),
    ("insight_kind", FieldType::Keyword, false),
    ("certainty", FieldType::Keyword, false),
    ("area", FieldType::Keyword, false),
];

/// Умолчания Qdrant 1.19 (`config/config.yaml`): `optimizers.flush_interval_sec` и
/// `wal.wal_capacity_mb` — если в конфигурации коллекции поля нет.
const QDRANT_FLUSH_INTERVAL_SEC: u64 = 5;
const QDRANT_WAL_CAPACITY_MB: u64 = 32;

/// Параметры WAL и сброса коллекции (задача 19 M3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CollectionTuning {
    /// `optimizers.flush_interval_sec`.
    pub flush_interval_sec: u64,
    /// `wal.wal_capacity_mb` (только при создании коллекции).
    pub wal_capacity_mb: u64,
}

/// Последняя запись в Qdrant (upsert, удаление, схема) — общая у клиентов одного
/// супервизора: время и наибольший номер операции WAL (`operation_id` ответов upsert и
/// удаления точек).
#[derive(Debug, Clone, Default)]
pub struct WriteMark(Arc<Mutex<MarkState>>);

#[derive(Debug, Default)]
struct MarkState {
    at: Option<Instant>,
    op: Option<u64>,
}

impl WriteMark {
    fn state(&self) -> std::sync::MutexGuard<'_, MarkState> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Отметить запись сейчас.
    pub fn touch(&self) {
        self.state().at = Some(Instant::now());
    }

    /// Подтверждённая Qdrant операция с номером `op` (WAL).
    pub fn record_op(&self, op: u64) {
        let mut s = self.state();
        s.at = Some(Instant::now());
        s.op = Some(s.op.map_or(op, |o| o.max(op)));
    }

    /// Забыть номер операции WAL (коллекция удалена, нумерация новой — с начала).
    pub fn reset_ops(&self) {
        self.state().op = None;
    }

    /// Сколько прошло с последней записи; `None` — записей не было.
    pub fn since_last(&self) -> Option<Duration> {
        self.state().at.map(|t| t.elapsed())
    }

    /// Наибольший номер операции WAL среди записей точек; `None` — их не было.
    pub fn last_op(&self) -> Option<u64> {
        self.state().op
    }
}

/// Векторный индекс на Qdrant (gRPC, API-ключ в заголовке `api-key`).
pub struct QdrantIndex {
    client: Qdrant,
    /// Параметры коллекции; `None` — умолчания Qdrant (внешний Qdrant, тесты).
    tuning: Option<CollectionTuning>,
    writes: WriteMark,
    /// Точный поиск (см. модуль): только eval.
    exact: bool,
}

impl std::fmt::Debug for QdrantIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Конфигурация клиента содержит ключ — наружу не выводим.
        f.debug_struct("QdrantIndex").finish_non_exhaustive()
    }
}

impl QdrantIndex {
    /// Клиент к `grpc_url` (например `http://127.0.0.1:6334`). Соединение ленивое:
    /// доступность проверяет [`VectorIndex::health`]. Проверка совместимости версий клиента
    /// отключена (она печатает в stdout); версию сервера сверяет супервизор (задача 6).
    pub fn connect(grpc_url: &str, api_key: Option<&str>) -> Result<Self, IndexError> {
        let client = Qdrant::from_url(grpc_url)
            .api_key(api_key)
            .timeout(Duration::from_secs(30))
            .connect_timeout(CONNECT_TIMEOUT)
            .skip_compatibility_check()
            .build()
            .map_err(|e| map_err("connect", e))?;
        Ok(QdrantIndex {
            client,
            tuning: None,
            writes: WriteMark::default(),
            exact: false,
        })
    }

    /// Точный поиск без HNSW и квантования (см. модуль) — только для eval.
    pub fn with_exact_search(mut self, exact: bool) -> Self {
        self.exact = exact;
        self
    }

    /// Параметры поиска dense: int8 с rescoring по оригиналам; при точном поиске — полный
    /// перебор по оригиналам.
    fn dense_params(&self) -> SearchParamsBuilder {
        if self.exact {
            SearchParamsBuilder::default()
                .exact(true)
                .quantization(QuantizationSearchParamsBuilder::default().ignore(true))
        } else {
            SearchParamsBuilder::default()
                .quantization(QuantizationSearchParamsBuilder::default().rescore(true))
        }
    }

    /// Создавать коллекцию с `t` и обновлять у существующей `flush_interval_sec`.
    pub fn with_tuning(mut self, t: CollectionTuning) -> Self {
        self.tuning = Some(t);
        self
    }

    /// Номер операции WAL из ответа записи точек — в отметку записей.
    fn note_op(&self, r: &qdrant_client::qdrant::PointsOperationResponse) {
        match r.result.and_then(|u| u.operation_id) {
            Some(op) => self.writes.record_op(op),
            None => self.writes.touch(),
        }
    }

    /// Отмечать записи в общей отметке `mark` (супервизора).
    pub fn with_write_mark(mut self, mark: WriteMark) -> Self {
        self.writes = mark;
        self
    }

    async fn ensure_impl(&self, model_id: &str, dim: usize) -> Result<IndexMeta, IndexError> {
        let exists = self
            .client
            .collection_exists(COLLECTION)
            .await
            .map_err(|e| map_err("collection_exists", e))?;
        let mut created = false;
        if !exists {
            match self.create_collection(model_id, dim).await {
                Ok(()) => created = true,
                Err(e) => {
                    // Гонка с параллельным ensure: коллекцию успел создать другой вызов.
                    let now = self
                        .client
                        .collection_exists(COLLECTION)
                        .await
                        .map_err(|e| map_err("collection_exists", e))?;
                    if !now {
                        return Err(e);
                    }
                }
            }
        }
        let (mut meta, schema, current) = self.read_meta().await?;
        meta.created = created;
        // Недостающие payload-индексы (обрыв между созданием коллекции и индексов, коллекция
        // M2 без индексов полей записей) досоздаём только в «своей» коллекции: чужую модель
        // не трогаем. Создание индекса на существующей коллекции точки не трогает.
        if meta.model_id.as_deref() == Some(model_id) {
            let mut added = 0;
            for (field, ty, tenant) in PAYLOAD_INDEXES {
                if !schema.iter().any(|s| s == field) {
                    self.create_index(field, ty, tenant).await?;
                    added += 1;
                }
            }
            if added > 0 && !created {
                tracing::info!(collection = COLLECTION, added, "добавлены payload-индексы");
            }
            if let Some(t) = self.tuning {
                self.apply_tuning(t, current).await?;
            }
        }
        Ok(meta)
    }

    /// Параметры сброса существующей коллекции: `flush_interval_sec` — `update_collection`;
    /// другой размер блока WAL — только запись в журнал (меняется пересозданием коллекции).
    async fn apply_tuning(
        &self,
        t: CollectionTuning,
        current: CollectionTuning,
    ) -> Result<(), IndexError> {
        if current.flush_interval_sec != t.flush_interval_sec {
            self.writes.touch();
            self.client
                .update_collection(UpdateCollectionBuilder::new(COLLECTION).optimizers_config(
                    OptimizersConfigDiffBuilder::default().flush_interval_sec(t.flush_interval_sec),
                ))
                .await
                .map_err(|e| map_err("update_collection", e))?;
            tracing::info!(
                collection = COLLECTION,
                from = current.flush_interval_sec,
                to = t.flush_interval_sec,
                "flush_interval_sec коллекции обновлён"
            );
        }
        if current.wal_capacity_mb != t.wal_capacity_mb {
            tracing::info!(
                collection = COLLECTION,
                current = current.wal_capacity_mb,
                configured = t.wal_capacity_mb,
                "размер блока WAL коллекции отличается от qdrant.wal_capacity_mb: применится после пересоздания коллекции (zam-server qdrant reset)"
            );
        }
        Ok(())
    }

    async fn create_collection(&self, model_id: &str, dim: usize) -> Result<(), IndexError> {
        let mut vectors = VectorsConfigBuilder::default();
        vectors.add_named_vector_params(
            DENSE,
            VectorParamsBuilder::new(dim as u64, Distance::Cosine)
                .memory(Memory::Cold)
                .hnsw_config(
                    HnswConfigDiffBuilder::default()
                        .m(16)
                        .ef_construct(100)
                        .payload_m(16),
                )
                .quantization_config(
                    ScalarQuantizationBuilder::default()
                        .quantile(0.99)
                        .memory(Memory::Pinned),
                ),
        );
        let mut sparse = SparseVectorsConfigBuilder::default();
        sparse.add_named_vector_params(
            SPARSE,
            SparseVectorParamsBuilder::default()
                .index(SparseIndexConfigBuilder::default().memory(Memory::Cached)),
        );
        let mut optimizers =
            OptimizersConfigDiffBuilder::default().default_segment_number(SEGMENTS);
        let mut req = CreateCollectionBuilder::new(COLLECTION)
            .vectors_config(vectors)
            .sparse_vectors_config(sparse)
            .on_disk_payload(true);
        if let Some(t) = self.tuning {
            optimizers = optimizers.flush_interval_sec(t.flush_interval_sec);
            req =
                req.wal_config(WalConfigDiffBuilder::default().wal_capacity_mb(t.wal_capacity_mb));
        }
        let mut req = req.optimizers_config(optimizers).build();
        // Метаданные кладём в поле напрямую: `.metadata()` билдера требует фичу `serde`.
        req.metadata = HashMap::from([
            ("model_id".to_string(), Value::from(model_id)),
            ("schema".to_string(), Value::from(SCHEMA_VERSION)),
        ]);
        self.writes.touch();
        self.client
            .create_collection(req)
            .await
            .map_err(|e| map_err("create_collection", e))?;
        for (field, ty, tenant) in PAYLOAD_INDEXES {
            self.create_index(field, ty, tenant).await?;
        }
        tracing::info!(collection = COLLECTION, model_id, dim, "коллекция создана");
        Ok(())
    }

    async fn create_index(
        &self,
        field: &str,
        ty: FieldType,
        tenant: bool,
    ) -> Result<(), IndexError> {
        self.writes.touch();
        let mut req = CreateFieldIndexCollectionBuilder::new(COLLECTION, field, ty).wait(true);
        if ty == FieldType::Keyword {
            req = req.field_index_params(KeywordIndexParamsBuilder::default().is_tenant(tenant));
        }
        // операция над схемой тоже идёт через WAL: её номер — в отметку, иначе ожидание при
        // остановке закончилось бы на последнем upsert, а индекс повторился бы при старте
        let r = self
            .client
            .create_field_index(req)
            .await
            .map_err(|e| map_err("create_field_index", e))?;
        self.note_op(&r);
        Ok(())
    }

    /// Метаданные, размерность, число точек, список проиндексированных полей payload и
    /// текущие параметры сброса (нет поля — умолчание Qdrant).
    async fn read_meta(&self) -> Result<(IndexMeta, Vec<String>, CollectionTuning), IndexError> {
        let info = self
            .client
            .collection_info(COLLECTION)
            .await
            .map_err(|e| map_err("collection_info", e))?
            .result
            .ok_or_else(|| IndexError::Corrupt("collection_info без результата".into()))?;
        let config = info
            .config
            .ok_or_else(|| IndexError::Corrupt("collection_info без config".into()))?;
        let model_id = config.metadata.get("model_id").and_then(value_str);
        let current = CollectionTuning {
            flush_interval_sec: config
                .optimizer_config
                .as_ref()
                .and_then(|o| o.flush_interval_sec)
                .unwrap_or(QDRANT_FLUSH_INTERVAL_SEC),
            wal_capacity_mb: config
                .wal_config
                .as_ref()
                .and_then(|w| w.wal_capacity_mb)
                .unwrap_or(QDRANT_WAL_CAPACITY_MB),
        };
        let dim = match config
            .params
            .and_then(|p| p.vectors_config)
            .and_then(|v| v.config)
        {
            Some(VectorsConfigKind::ParamsMap(m)) => m.map.get(DENSE).map(|p| p.size as usize),
            _ => None,
        }
        .ok_or_else(|| IndexError::Corrupt("в коллекции нет вектора dense".into()))?;
        let points = self
            .client
            .count(CountPointsBuilder::new(COLLECTION).exact(true))
            .await
            .map_err(|e| map_err("count", e))?
            .result
            .map(|r| r.count)
            .unwrap_or(0);
        let schema = info.payload_schema.into_keys().collect();
        Ok((
            IndexMeta {
                model_id,
                dim,
                points,
                created: false,
            },
            schema,
            current,
        ))
    }

    async fn upsert_impl(&self, pts: Vec<IndexPoint>) -> Result<(), IndexError> {
        // Неизменное ограничение §6.4: Human никогда не попадает в Qdrant.
        check_points(&pts)?;
        let mut it = pts.into_iter().peekable();
        while it.peek().is_some() {
            let batch: Vec<PointStruct> = it.by_ref().take(UPSERT_BATCH).map(to_point).collect();
            self.writes.touch();
            let r = self
                .client
                .upsert_points(UpsertPointsBuilder::new(COLLECTION, batch).wait(true))
                .await
                .map_err(|e| map_err("upsert", e))?;
            self.note_op(&r);
        }
        Ok(())
    }

    async fn delete_targets_impl(&self, user: Id, targets: &[TargetRef]) -> Result<(), IndexError> {
        let (nodes, entries) = split_targets(targets);
        let mut any = Vec::new();
        if !nodes.is_empty() {
            any.push(kw_any("node_id", nodes));
        }
        if !entries.is_empty() {
            any.push(kw_any("entry_id", entries));
        }
        if any.is_empty() {
            return Ok(());
        }
        let filter = Filter {
            must: vec![kw("user_id", user.to_string())],
            should: any,
            ..Filter::default()
        };
        self.writes.touch();
        let r = self
            .client
            .delete_points(
                DeletePointsBuilder::new(COLLECTION)
                    .points(filter)
                    .wait(true),
            )
            .await
            .map_err(|e| map_err("delete", e))?;
        self.note_op(&r);
        Ok(())
    }

    async fn delete_scope_impl(&self, f: &VectorFilter) -> Result<(), IndexError> {
        // Фильтр без областей Qdrant понял бы как «все точки»: удалять нечего.
        let Some(filter) = point_filter(f, &f.levels) else {
            return Ok(());
        };
        self.writes.touch();
        let r = self
            .client
            .delete_points(
                DeletePointsBuilder::new(COLLECTION)
                    .points(filter)
                    .wait(true),
            )
            .await
            .map_err(|e| map_err("delete", e))?;
        self.note_op(&r);
        Ok(())
    }

    async fn search_impl(
        &self,
        q_dense: &[f32],
        q_sparse: &[(u32, f32)],
        f: &VectorFilter,
    ) -> Result<Vec<VectorHit>, IndexError> {
        let leaf_levels = allowed_levels(&[LEVEL_CHUNK], &f.levels);
        let note_levels = allowed_levels(&LEVELS_NOTE, &f.levels);
        let (Some(leaf_filter), Some(note_filter)) =
            (point_filter(f, &leaf_levels), point_filter(f, &note_levels))
        else {
            return Ok(Vec::new());
        };

        let mut q = QueryPointsBuilder::new(COLLECTION);
        let mut prefetches = 0;
        if !leaf_levels.is_empty() && !q_dense.is_empty() {
            q = q.add_prefetch(
                PrefetchQueryBuilder::default()
                    .query(Query::new_nearest(q_dense.to_vec()))
                    .using(DENSE)
                    .filter(leaf_filter.clone())
                    .params(self.dense_params())
                    .limit(LEAF_PREFETCH),
            );
            prefetches += 1;
        }
        if !leaf_levels.is_empty() && !q_sparse.is_empty() {
            let mut sparse = PrefetchQueryBuilder::default()
                .query(Query::new_nearest(VectorInput::from(q_sparse)))
                .using(SPARSE)
                .filter(leaf_filter)
                .limit(LEAF_PREFETCH);
            if self.exact {
                sparse = sparse.params(SearchParamsBuilder::default().exact(true));
            }
            q = q.add_prefetch(sparse);
            prefetches += 1;
        }
        if !note_levels.is_empty() && !q_dense.is_empty() {
            q = q.add_prefetch(
                PrefetchQueryBuilder::default()
                    .query(Query::new_nearest(q_dense.to_vec()))
                    .using(DENSE)
                    .filter(note_filter)
                    .params(self.dense_params())
                    .limit(NOTE_PREFETCH),
            );
            prefetches += 1;
        }
        // точки папок — своим prefetch; лимит слияния растёт на его размер: папки не
        // занимают места чанков (каждая точка — только в одном prefetch, RRF не меняет
        // взаимный порядок остальных)
        let mut limit = QUERY_LIMIT;
        if f.folders
            && !q_dense.is_empty()
            && let Some(folder_filter) = point_filter(&f.folder_filter(), &[LEVEL_FOLDER.into()])
        {
            q = q.add_prefetch(
                PrefetchQueryBuilder::default()
                    .query(Query::new_nearest(q_dense.to_vec()))
                    .using(DENSE)
                    .filter(folder_filter)
                    .params(self.dense_params())
                    .limit(FOLDER_PREFETCH),
            );
            prefetches += 1;
            limit += FOLDER_PREFETCH;
        }
        if prefetches == 0 {
            return Ok(Vec::new());
        }
        let q = q
            .query(Query::new_rrf(RrfBuilder::with_k(RRF_K)))
            .limit(limit)
            .with_payload(hit_payload());
        let resp = self
            .client
            .query(q)
            .await
            .map_err(|e| map_err("query", e))?;
        resp.result.into_iter().map(to_hit).collect()
    }

    async fn nearest_dense_impl(
        &self,
        v: &[f32],
        f: &VectorFilter,
        limit: usize,
    ) -> Result<Vec<VectorHit>, IndexError> {
        let Some(filter) = point_filter(f, &f.levels) else {
            return Ok(Vec::new());
        };
        if v.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let q = QueryPointsBuilder::new(COLLECTION)
            .query(Query::new_nearest(v.to_vec()))
            .using(DENSE)
            .filter(filter)
            .params(self.dense_params())
            .limit(limit as u64)
            .with_payload(hit_payload());
        let resp = self
            .client
            .query(q)
            .await
            .map_err(|e| map_err("query", e))?;
        resp.result.into_iter().map(to_hit).collect()
    }

    async fn count_impl(&self, f: &VectorFilter) -> Result<u64, IndexError> {
        let Some(filter) = point_filter(f, &f.levels) else {
            return Ok(0);
        };
        Ok(self
            .client
            .count(
                CountPointsBuilder::new(COLLECTION)
                    .filter(filter)
                    .exact(true),
            )
            .await
            .map_err(|e| map_err("count", e))?
            .result
            .map(|r| r.count)
            .unwrap_or(0))
    }

    async fn drop_all_impl(&self) -> Result<(), IndexError> {
        let exists = self
            .client
            .collection_exists(COLLECTION)
            .await
            .map_err(|e| map_err("collection_exists", e))?;
        if exists {
            self.writes.touch();
            self.client
                .delete_collection(DeleteCollectionBuilder::new(COLLECTION))
                .await
                .map_err(|e| map_err("delete_collection", e))?;
            // у новой коллекции нумерация WAL с начала: прежний максимум держал бы
            // `wal_pending_ops` и ожидание при остановке на старом номере
            self.writes.reset_ops();
            tracing::info!(collection = COLLECTION, "коллекция удалена");
        }
        Ok(())
    }

    async fn folder_hashes_impl(&self, user: Id) -> Result<Option<Vec<(Id, String)>>, IndexError> {
        let filter = Filter::must([
            kw("user_id", user.to_string()),
            kw("level", LEVEL_FOLDER.to_string()),
        ]);
        let mut out = Vec::new();
        let mut offset: Option<qdrant_client::qdrant::PointId> = None;
        loop {
            let mut req = ScrollPointsBuilder::new(COLLECTION)
                .filter(filter.clone())
                .limit(1000)
                .with_payload(SelectorOptions::Include(PayloadIncludeSelector {
                    fields: vec!["node_id".into(), "text_hash".into()],
                }))
                .with_vectors(false);
            if let Some(o) = offset.take() {
                req = req.offset(o);
            }
            let resp = self
                .client
                .scroll(req)
                .await
                .map_err(|e| map_err("scroll", e))?;
            for p in resp.result {
                let node = p
                    .payload
                    .get("node_id")
                    .and_then(value_str)
                    .and_then(|s| s.parse::<Id>().ok());
                let hash = p.payload.get("text_hash").and_then(value_str);
                if let (Some(n), Some(h)) = (node, hash) {
                    out.push((n, h));
                }
            }
            match resp.next_page_offset {
                Some(o) => offset = Some(o),
                None => break,
            }
        }
        out.sort();
        Ok(Some(out))
    }

    async fn health_impl(&self) -> Result<(), IndexError> {
        // health_check в gRPC доступен без ключа; list_collections проверяет и ключ.
        self.client
            .list_collections()
            .await
            .map_err(|e| map_err("health", e))?;
        Ok(())
    }
}

impl VectorIndex for QdrantIndex {
    fn ensure<'a>(&'a self, model_id: &'a str, dim: usize) -> IndexFuture<'a, IndexMeta> {
        Box::pin(self.ensure_impl(model_id, dim))
    }

    fn upsert(&self, pts: Vec<IndexPoint>) -> IndexFuture<'_, ()> {
        Box::pin(self.upsert_impl(pts))
    }

    fn delete_targets<'a>(&'a self, user: Id, targets: &'a [TargetRef]) -> IndexFuture<'a, ()> {
        Box::pin(self.delete_targets_impl(user, targets))
    }

    fn delete_scope<'a>(&'a self, f: &'a VectorFilter) -> IndexFuture<'a, ()> {
        Box::pin(self.delete_scope_impl(f))
    }

    fn search<'a>(
        &'a self,
        q_dense: &'a [f32],
        q_sparse: &'a [(u32, f32)],
        f: &'a VectorFilter,
    ) -> IndexFuture<'a, Vec<VectorHit>> {
        Box::pin(self.search_impl(q_dense, q_sparse, f))
    }

    fn nearest_dense<'a>(
        &'a self,
        v: &'a [f32],
        f: &'a VectorFilter,
        limit: usize,
    ) -> IndexFuture<'a, Vec<VectorHit>> {
        Box::pin(self.nearest_dense_impl(v, f, limit))
    }

    fn count<'a>(&'a self, f: &'a VectorFilter) -> IndexFuture<'a, u64> {
        Box::pin(self.count_impl(f))
    }

    fn drop_all(&self) -> IndexFuture<'_, ()> {
        Box::pin(self.drop_all_impl())
    }

    fn health(&self) -> IndexFuture<'_, ()> {
        Box::pin(self.health_impl())
    }

    fn folder_hashes(&self, user: Id) -> IndexFuture<'_, Option<Vec<(Id, String)>>> {
        Box::pin(self.folder_hashes_impl(user))
    }
}

/// Точка Qdrant: id — UUIDv5 от `chunk_id`, `chunk_id` хранится в payload для выдачи.
fn to_point(p: IndexPoint) -> PointStruct {
    let mut vectors = NamedVectors::default().add_vector(DENSE, Vector::new_dense(p.dense));
    if !p.sparse.is_empty() {
        let (idx, val): (Vec<u32>, Vec<f32>) = p.sparse.into_iter().unzip();
        vectors = vectors.add_vector(SPARSE, Vector::new_sparse(idx, val));
    }
    let pl = p.payload;
    let mut payload = Payload::new();
    payload.insert("chunk_id", p.chunk_id.clone());
    payload.insert("user_id", pl.user_id.to_string());
    payload.insert("space_id", pl.space_id.to_string());
    payload.insert("space_kind", pl.space_kind);
    if let Some(g) = pl.solution_guid {
        payload.insert("solution_guid", g);
    }
    if let Some(n) = pl.node_id {
        payload.insert("node_id", n.to_string());
    }
    if let Some(e) = pl.entry_id {
        payload.insert("entry_id", e.to_string());
    }
    payload.insert("path_ancestors", pl.path_ancestors);
    if let Some(a) = pl.area {
        payload.insert("area", a);
    }
    payload.insert("level", pl.level);
    payload.insert("kind", pl.kind);
    payload.insert("tags", pl.tags);
    payload.insert("updated_at", pl.updated_at);
    payload.insert("rev", pl.rev);
    payload.insert("text_hash", pl.text_hash);
    for (k, v) in [
        ("status", pl.status),
        ("insight_kind", pl.insight_kind),
        ("certainty", pl.certainty),
    ] {
        if let Some(v) = v {
            payload.insert(k, v);
        }
    }
    PointStruct::new(point_id(&p.chunk_id).to_string(), vectors, payload)
}

/// Точное совпадение keyword. `MatchValue` строим явно: `From<String>` у клиента
/// превращает строку с пробелом в полнотекстовый match (пути с пробелами).
fn kw(field: &str, value: String) -> Condition {
    Condition::matches(field, MatchValue::Keyword(value))
}

/// Совпадение с любым из значений keyword.
fn kw_any(field: &str, values: Vec<String>) -> Condition {
    Condition::matches(field, values)
}

/// Тенант и пространства одной области.
fn scope_must(s: &VectorScope) -> Vec<Condition> {
    vec![
        kw("user_id", s.user_id.to_string()),
        kw_any("space_id", s.space_ids.iter().map(Id::to_string).collect()),
    ]
}

/// Фильтр точек: дизъюнкция областей (одна область — прямо в `must`, условие на тенанта
/// остаётся верхнеуровневым), путь, уровни `levels` (пустой — все), виды, виды инсайтов,
/// `certainty` и `area` — в `must`, исключаемые статусы — в `must_not` (точки без `status`
/// проходят). `None` — нет ни одной области с пространствами: фильтр не пропускает ничего
/// (пустой фильтр Qdrant пропустил бы всё).
fn point_filter(f: &VectorFilter, levels: &[String]) -> Option<Filter> {
    let scopes: Vec<&VectorScope> = f.live_scopes().collect();
    let mut filter = Filter::default();
    match scopes.as_slice() {
        [] => return None,
        [one] => filter.must = scope_must(one),
        many => {
            filter.should = many
                .iter()
                .map(|s| Condition::from(Filter::must(scope_must(s))))
                .collect();
        }
    }
    if let Some(prefix) = &f.path_prefix {
        filter.must.push(kw("path_ancestors", prefix.clone()));
    }
    for (field, values) in [
        ("level", levels),
        ("kind", f.kinds.as_slice()),
        ("insight_kind", f.insight_kinds.as_slice()),
        ("certainty", f.certainty.as_slice()),
    ] {
        if !values.is_empty() {
            filter.must.push(kw_any(field, values.to_vec()));
        }
    }
    if let Some(area) = &f.area {
        filter.must.push(kw("area", area.clone()));
    }
    if !f.exclude_statuses.is_empty() {
        filter
            .must_not
            .push(kw_any("status", f.exclude_statuses.clone()));
    }
    Some(filter)
}

/// Поля payload для выдачи.
fn hit_payload() -> SelectorOptions {
    SelectorOptions::Include(PayloadIncludeSelector {
        fields: HIT_FIELDS.iter().map(|f| (*f).to_string()).collect(),
    })
}

/// Точка выдачи → [`VectorHit`].
fn to_hit(p: ScoredPoint) -> Result<VectorHit, IndexError> {
    let get = |k: &str| p.payload.get(k).and_then(value_str);
    let chunk_id =
        get("chunk_id").ok_or_else(|| IndexError::Corrupt("точка без chunk_id".into()))?;
    let user_id = get("user_id")
        .and_then(|s| s.parse::<Id>().ok())
        .ok_or_else(|| IndexError::Corrupt("точка без корректного user_id".into()))?;
    let target = hit_target(get("node_id"), get("entry_id"))?;
    let level = get("level").ok_or_else(|| IndexError::Corrupt("точка без level".into()))?;
    Ok(VectorHit {
        chunk_id,
        user_id,
        target,
        score: p.score,
        level,
    })
}

/// Уровни prefetch с учётом фильтра вызывающего (пустой фильтр — все уровни prefetch).
fn allowed_levels(prefetch: &[&str], requested: &[String]) -> Vec<String> {
    prefetch
        .iter()
        .filter(|l| requested.is_empty() || requested.iter().any(|r| r == *l))
        .map(|l| (*l).to_string())
        .collect()
}

fn value_str(v: &Value) -> Option<String> {
    match &v.kind {
        Some(Kind::StringValue(s)) => Some(s.clone()),
        _ => None,
    }
}

/// Ошибка клиента → [`IndexError`]. В текст попадают операция, код и усечённое сообщение
/// сервера; метаданные статуса (заголовки, в т.ч. ключ) не выводятся.
fn map_err(op: &'static str, e: QdrantError) -> IndexError {
    match e {
        QdrantError::ResponseError { status } => {
            let msg = truncate(status.message());
            match status.code() {
                Code::Unauthenticated | Code::PermissionDenied => IndexError::Auth,
                Code::Unavailable
                | Code::DeadlineExceeded
                | Code::Cancelled
                | Code::ResourceExhausted => IndexError::Unavailable(format!("{op}: {msg}")),
                Code::NotFound => IndexError::NotFound(format!("{op}: {msg}")),
                // Ошибка соединения клиент оборачивает в Internal («Failed to connect to …»).
                Code::Internal if msg.starts_with("Failed to connect") => {
                    IndexError::Unavailable(format!("{op}: {msg}"))
                }
                Code::DataLoss => IndexError::Corrupt(format!("{op}: {msg}")),
                code => IndexError::Other(format!("{op}: {code:?}: {msg}")),
            }
        }
        QdrantError::ResourceExhaustedError {
            retry_after_seconds,
            ..
        } => IndexError::Unavailable(format!(
            "{op}: перегрузка, повтор через {retry_after_seconds} с"
        )),
        QdrantError::Io(e) => IndexError::Unavailable(format!("{op}: {e}")),
        QdrantError::InvalidUri(_) => IndexError::Other(format!("{op}: неверный адрес Qdrant")),
        other => IndexError::Other(format!("{op}: {}", truncate(&other.to_string()))),
    }
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
    fn allowed_levels_intersects_with_request() {
        assert_eq!(allowed_levels(&LEVELS_NOTE, &[]), ["note", "section"]);
        assert_eq!(
            allowed_levels(&LEVELS_NOTE, &["note".to_string(), "chunk".to_string()]),
            ["note"]
        );
        assert!(allowed_levels(&[LEVEL_CHUNK], &["note".to_string()]).is_empty());
    }

    #[test]
    fn keyword_with_space_stays_keyword() {
        let c = kw("path_ancestors", "my notes".into());
        let dbg = format!("{c:?}");
        assert!(dbg.contains("Keyword(\"my notes\")"), "{dbg}");
    }

    #[test]
    fn status_codes_map_to_errors() {
        let st = |c: Code| QdrantError::ResponseError {
            status: tonic::Status::new(c, "m"),
        };
        assert!(matches!(
            map_err("t", st(Code::Unauthenticated)),
            IndexError::Auth
        ));
        assert!(matches!(
            map_err("t", st(Code::PermissionDenied)),
            IndexError::Auth
        ));
        assert!(matches!(
            map_err("t", st(Code::Unavailable)),
            IndexError::Unavailable(_)
        ));
        assert!(matches!(
            map_err("t", st(Code::DeadlineExceeded)),
            IndexError::Unavailable(_)
        ));
        assert!(matches!(
            map_err("t", st(Code::ResourceExhausted)),
            IndexError::Unavailable(_)
        ));
        assert!(matches!(
            map_err("t", st(Code::NotFound)),
            IndexError::NotFound(_)
        ));
        assert!(matches!(
            map_err("t", st(Code::InvalidArgument)),
            IndexError::Other(_)
        ));
    }

    #[test]
    fn write_mark_keeps_max_op_and_resets() {
        let m = WriteMark::default();
        assert_eq!((m.last_op(), m.since_last()), (None, None));
        m.record_op(5);
        m.record_op(3);
        assert_eq!(m.last_op(), Some(5));
        // коллекция пересоздана: нумерация WAL с начала, время записи остаётся
        m.reset_ops();
        assert_eq!(m.last_op(), None);
        assert!(m.since_last().is_some());
        m.record_op(2);
        assert_eq!(m.last_op(), Some(2));
    }

    #[test]
    fn truncate_limits_message() {
        let long = "я".repeat(500);
        assert_eq!(truncate(&long).chars().count(), MAX_ERR_MSG + 1);
        assert_eq!(truncate("ok"), "ok");
    }
}
