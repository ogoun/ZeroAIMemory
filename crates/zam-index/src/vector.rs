//! Типы и трейт векторного индекса, не зависящие от Qdrant.

use std::future::Future;
use std::pin::Pin;

use uuid::Uuid;
use zam_core::Id;

use crate::IndexError;

/// Пространство имён UUIDv5 для id точек ZeroAIMemory. Значение фиксировано навсегда:
/// его смена меняет id всех точек и требует полной переиндексации.
/// Получено как UUIDv5(`NAMESPACE_URL`, "https://zeroaimemory/qdrant/point").
pub const NAMESPACE_ZAM: Uuid = Uuid::from_u128(0x0760_4f2f_80d1_5f87_a58c_9d03_adae_a600);

/// Детерминированный id точки: UUIDv5(`NAMESPACE_ZAM`, `chunk_id`).
/// Повторный upsert того же чанка перезаписывает точку, а не создаёт новую.
pub fn point_id(chunk_id: &str) -> Uuid {
    Uuid::new_v5(&NAMESPACE_ZAM, chunk_id.as_bytes())
}

/// Payload точки (§6.4). Строковые id — в base32 (`Id::to_string`).
#[derive(Debug, Clone, PartialEq)]
pub struct PointPayload {
    /// Тенант: обязателен в каждом запросе.
    pub user_id: Id,
    pub space_id: Id,
    /// `rules` / `knowledge` / `solution`; `human` отклоняется адаптером.
    pub space_kind: String,
    pub solution_guid: Option<String>,
    pub node_id: Id,
    /// Все префиксы пути узла: "a", "a/b", "a/b/c" — для фильтра `path_prefix`.
    pub path_ancestors: Vec<String>,
    pub area: Option<String>,
    /// `chunk` / `section` / `note`.
    pub level: String,
    /// Вид узла, в M2 — `note`.
    pub kind: String,
    pub tags: Vec<String>,
    /// Unix-время, секунды.
    pub updated_at: i64,
    pub rev: i64,
    pub text_hash: String,
}

/// Точка индекса: чанк с dense- и sparse-векторами.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexPoint {
    pub chunk_id: String,
    pub dense: Vec<f32>,
    /// Пары (индекс токена, вес); пустой — точка без sparse-вектора.
    pub sparse: Vec<(u32, f32)>,
    pub payload: PointPayload,
}

/// Фильтр поиска. `user_id` присутствует всегда (тенант).
#[derive(Debug, Clone, PartialEq)]
pub struct VectorFilter {
    pub user_id: Id,
    /// Пространства (match any). Пустой — без ограничения по пространству в пределах пользователя.
    pub space_ids: Vec<Id>,
    /// Префикс пути по целым сегментам: совпадение с одним из `path_ancestors`.
    pub path_prefix: Option<String>,
    /// Уровни (match any). Пустой — все уровни по схеме prefetch.
    pub levels: Vec<String>,
}

/// Результат поиска: score — RRF-оценка слияния.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorHit {
    pub chunk_id: String,
    pub node_id: Id,
    pub score: f32,
    pub level: String,
}

/// Состояние коллекции, которое возвращает [`VectorIndex::ensure`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexMeta {
    /// `model_id` из метаданных коллекции; `None` — метаданных нет (коллекция не наша).
    pub model_id: Option<String>,
    /// Размерность вектора `dense` в коллекции.
    pub dim: usize,
    /// Точное число точек.
    pub points: u64,
    /// Коллекция создана этим вызовом.
    pub created: bool,
}

/// Future методов [`VectorIndex`]. Трейт возвращает boxed-future, чтобы оставаться
/// dyn-совместимым (`Arc<dyn VectorIndex>`): `async fn` в трейте и `impl Future` в
/// возвращаемом типе делают его не dyn-совместимым, а `async-trait` даёт то же самое
/// через макрос и лишнюю зависимость. Одна аллокация на вызов несущественна рядом с gRPC.
pub type IndexFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, IndexError>> + Send + 'a>>;

/// Векторный индекс. Все записи подтверждаются (`wait=true`): задание индексации
/// закрывается только после успешного ответа.
pub trait VectorIndex: Send + Sync {
    /// Создаёт коллекцию с метаданными `{model_id, schema}`, если её нет.
    /// Существующую коллекцию с другим `model_id` или `dim` не трогает и возвращает её
    /// состояние: решение (выключить векторный канал до reindex) принимает вызывающий.
    fn ensure<'a>(&'a self, model_id: &'a str, dim: usize) -> IndexFuture<'a, IndexMeta>;

    /// Upsert точек пачками, `wait=true`. Точки пространства `human` отклоняются целиком.
    fn upsert(&self, pts: Vec<IndexPoint>) -> IndexFuture<'_, ()>;

    /// Удаляет все точки узла пользователя (по фильтру `user_id` + `node_id`), `wait=true`.
    fn delete_node(&self, user: Id, node: Id) -> IndexFuture<'_, ()>;

    /// Гибридный поиск: prefetch dense и sparse по чанкам + dense по заметкам и секциям,
    /// слияние RRF.
    fn search<'a>(
        &'a self,
        q_dense: &'a [f32],
        q_sparse: &'a [(u32, f32)],
        f: &'a VectorFilter,
    ) -> IndexFuture<'a, Vec<VectorHit>>;

    /// Точное число точек по фильтру: тенант, пространства, префикс пути; пустой
    /// `levels` — все уровни (для `/index/status` и проверок исключений).
    fn count<'a>(&'a self, f: &'a VectorFilter) -> IndexFuture<'a, u64>;

    /// Удаляет коллекцию целиком (reindex, смена модели). Идемпотентно.
    fn drop_all(&self) -> IndexFuture<'_, ()>;

    /// Проверка доступности и ключа (аутентифицированный запрос).
    fn health(&self) -> IndexFuture<'_, ()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespace_is_documented_value() {
        let expected = Uuid::new_v5(&Uuid::NAMESPACE_URL, b"https://zeroaimemory/qdrant/point");
        assert_eq!(NAMESPACE_ZAM, expected);
    }

    #[test]
    fn point_id_stable_known_vector() {
        assert_eq!(point_id("x"), Uuid::new_v5(&NAMESPACE_ZAM, b"x"));
        assert_ne!(point_id("x"), point_id("y"));
    }
}
