//! Векторный индекс ZeroAIMemory (§6.4, исследование 05 §5–6).
//!
//! - [`VectorIndex`] — трейт индекса: на нём строятся супервизор (задача 6), воркер
//!   индексации (задача 7), гибридный поиск (задача 8), reindex (задача 10) и Edge-прототип
//!   (задача 16). Используется как `Arc<dyn VectorIndex>`.
//! - [`QdrantIndex`] — адаптер к локальному Qdrant 1.19 по gRPC (`qdrant-client`):
//!   коллекция `zam_chunks` с именованными векторами `dense` и `sparse`, гибридный запрос
//!   prefetch + RRF, фильтр по тенанту `user_id`.
//! - [`point_id`] — детерминированный id точки (UUIDv5 от `chunk_id`): upsert идемпотентен.
//! - [`supervisor`] — супервизор нативного Qdrant (§4.3): запуск с привязкой к родителю
//!   (Job Object / PDEATHSIG), готовность и версия, перезапуск с circuit breaker, метка
//!   хранилища и пересборка, штатная остановка; `status`/`reset` для CLI.
//!
//! - [`worker`] — воркер индексации: outbox `index_jobs` → кэш `embeddings` → инференс →
//!   upsert в Qdrant → `index_state`; свёртка L0 ([`rollup`], §8.3); сборка мусора кэша.
//! - [`reindex`] — пересборка области из кэша `embeddings` без инференса (или полная):
//!   выполняется воркером по запросу и командой `zam-server reindex`.
//!
//! - [`EdgeIndex`] (feature `edge`, не по умолчанию) — прототип того же трейта на встроенном
//!   Qdrant Edge для сравнения с managed-вариантом (задача 16, ADR 0001).
//!
//! Источник истины — SQLite: индекс пересобирается из `embeddings` без инференса.
//! Пространство `human` в индекс никогда не попадает. В ошибки и логи не попадают
//! векторы, текст чанков и API-ключ.

#[cfg(feature = "edge")]
mod edge;
mod qdrant;
pub mod reindex;
pub mod rollup;
pub mod supervisor;
#[cfg(feature = "test-hooks")]
mod test_hooks;
mod vector;
pub mod worker;

#[cfg(feature = "edge")]
pub use edge::{EdgeIndex, EdgeOptions};
pub use qdrant::{COLLECTION, DENSE, QdrantIndex, SCHEMA_VERSION, SPARSE};
pub use reindex::{ReindexCtx, ReindexReport, ReindexRequest, reindex};
pub use vector::{
    IndexFuture, IndexMeta, IndexPoint, NAMESPACE_ZAM, PointPayload, VectorFilter, VectorHit,
    VectorIndex, VectorScope, point_id,
};
pub use worker::{
    PassageEmbedder, ReindexOutcome, SkippedDb, WorkerConfig, WorkerDeps, WorkerHandle,
    WorkerStatus, embed_input, embed_input_of, embed_key, embed_key_of,
};

/// Ошибки векторного индекса. Тексты без векторов, текста чанков и секретов.
#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    /// Qdrant недоступен (нет соединения, таймаут, перегрузка): поиск переходит в `degraded`,
    /// задания индексации копятся и догоняются после возврата Qdrant.
    #[error("Qdrant недоступен: {0}")]
    Unavailable(String),
    /// Неверный или отсутствующий API-ключ.
    #[error("Qdrant: ошибка аутентификации")]
    Auth,
    /// Коллекции нет (стёрта, удалена `drop_all` или ещё не создана): воркер вызывает
    /// [`VectorIndex::ensure`] и повторяет операцию, задание не считается неудачным.
    #[error("Qdrant: коллекция не найдена: {0}")]
    NotFound(String),
    /// Коллекция или ответ в неожиданном виде (нет вектора `dense`, нет полей payload).
    #[error("индекс повреждён: {0}")]
    Corrupt(String),
    /// Каталог `<data_dir>/qdrant/` занят другим супервизором (второй экземпляр сервера).
    #[error("каталог Qdrant {0} занят другим экземпляром zam-server")]
    Locked(String),
    /// Ошибка управления процессом Qdrant (каталог, поток, стирание хранилища).
    #[error("супервизор Qdrant: {0}")]
    Process(String),
    /// Прочие ошибки (неверный запрос, отклонённые данные).
    #[error("ошибка индекса: {0}")]
    Other(String),
}
