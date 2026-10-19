//! Векторный индекс ZeroAIMemory (§6.4, исследование 05 §5–6).
//!
//! - [`VectorIndex`] — трейт индекса: на нём строятся супервизор (задача 6), воркер
//!   индексации (задача 7), гибридный поиск (задача 8), reindex (задача 10) и Edge-прототип
//!   (задача 16). Используется как `Arc<dyn VectorIndex>`.
//! - [`QdrantIndex`] — адаптер к локальному Qdrant 1.19 по gRPC (`qdrant-client`):
//!   коллекция `zam_chunks` с именованными векторами `dense` и `sparse`, гибридный запрос
//!   prefetch + RRF, фильтр по тенанту `user_id`.
//! - [`point_id`] — детерминированный id точки (UUIDv5 от `chunk_id`): upsert идемпотентен.
//!
//! Источник истины — SQLite: индекс пересобирается из `embeddings` без инференса.
//! Пространство `human` в индекс никогда не попадает. В ошибки и логи не попадают
//! векторы, текст чанков и API-ключ.

mod qdrant;
mod vector;

pub use qdrant::{COLLECTION, DENSE, QdrantIndex, SCHEMA_VERSION, SPARSE};
pub use vector::{
    IndexFuture, IndexMeta, IndexPoint, NAMESPACE_ZAM, PointPayload, VectorFilter, VectorHit,
    VectorIndex, point_id,
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
    /// Коллекция или ответ в неожиданном виде (нет вектора `dense`, нет полей payload).
    #[error("индекс повреждён: {0}")]
    Corrupt(String),
    /// Прочие ошибки (неверный запрос, отклонённые данные).
    #[error("ошибка индекса: {0}")]
    Other(String),
}
