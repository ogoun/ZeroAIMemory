//! Типы и трейт векторного индекса, не зависящие от Qdrant.

use std::future::Future;
use std::pin::Pin;

use uuid::Uuid;
use zam_core::{Id, TargetKind, TargetRef};

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

/// Payload точки (§6.4). Строковые id — в base32 (`Id::to_string`). Цель точки — ровно
/// одно из `node_id` (узел) и `entry_id` (запись журнала): точку без цели или с обеими
/// целями адаптер отклоняет. Поля записей (`entry_id`, `status`, `insight_kind`,
/// `certainty`) добавлены в M3 без пересборки коллекции: у точек M2 их нет.
#[derive(Debug, Clone, PartialEq)]
pub struct PointPayload {
    /// Тенант — владелец базы, в которой лежит узел или запись; обязателен в каждом запросе.
    pub user_id: Id,
    pub space_id: Id,
    /// `rules` / `knowledge` / `solution`; `human` отклоняется адаптером.
    pub space_kind: String,
    pub solution_guid: Option<String>,
    pub node_id: Option<Id>,
    pub entry_id: Option<Id>,
    /// Все префиксы пути узла: "a", "a/b", "a/b/c" — для фильтра `path_prefix`.
    pub path_ancestors: Vec<String>,
    pub area: Option<String>,
    /// `chunk` / `section` / `note` / `folder`.
    pub level: String,
    /// Вид: `note` / `folder` у узлов, `insight` / `changelog` / `release` у записей.
    pub kind: String,
    pub tags: Vec<String>,
    /// Unix-время, секунды.
    pub updated_at: i64,
    pub rev: i64,
    pub text_hash: String,
    /// Статус записи: `active` / `revisit` / `superseded` / `retracted`.
    pub status: Option<String>,
    /// Вид инсайта (`data.kind` записи `insight`).
    pub insight_kind: Option<String>,
    /// `axiom` / `theorem` / `hypothesis`.
    pub certainty: Option<String>,
}

impl PointPayload {
    /// Цель точки; `None` — нет ни одной цели или заданы обе.
    pub fn target(&self) -> Option<TargetRef> {
        match (self.node_id, self.entry_id) {
            (Some(id), None) => Some(TargetRef {
                kind: TargetKind::Node,
                id,
            }),
            (None, Some(id)) => Some(TargetRef {
                kind: TargetKind::Entry,
                id,
            }),
            _ => None,
        }
    }
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

/// Одна область: база владельца (тенант `user_id`) и её пространства. Список пространств
/// должен быть непустым: область без пространств не пропускает ни одной точки (а не «все
/// пространства владельца») — ошибка вызывающего не открывает чужую базу целиком.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VectorScope {
    pub user_id: Id,
    pub space_ids: Vec<Id>,
}

/// Фильтр поиска, подсчёта и удаления. Области — дизъюнкция (`should`), остальные условия
/// — конъюнкция поверх неё, то есть действуют внутри каждой области. Пустой список областей
/// (или только области без пространств) — пустая выдача, счёт 0, удаление ничего не трогает.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VectorFilter {
    pub scopes: Vec<VectorScope>,
    /// Префикс пути по целым сегментам: совпадение с одним из `path_ancestors`.
    pub path_prefix: Option<String>,
    /// Уровни (match any). Пустой — все уровни (в гибридном поиске — по схеме prefetch).
    pub levels: Vec<String>,
    /// Виды (`kind`, match any). Пустой — без ограничения.
    pub kinds: Vec<String>,
    /// Исключаемые статусы (`must_not`): точки без `status` (узлы, точки M2) проходят.
    pub exclude_statuses: Vec<String>,
    /// Виды инсайтов (match any). Пустой — без ограничения.
    pub insight_kinds: Vec<String>,
    /// Уровни `certainty` (match any); условие задаётся, только если список непуст.
    pub certainty: Vec<String>,
    /// Область знаний (`area`, точное совпадение).
    pub area: Option<String>,
    /// Гибридный поиск ([`VectorIndex::search`]) добавляет prefetch dense по точкам папок
    /// (`level = kind = folder`, канал папок §8.3): фильтр точек папок — [`Self::folder_filter`].
    /// Остальные методы поле не читают.
    pub folders: bool,
}

impl VectorFilter {
    /// Фильтр одной области без прочих условий.
    pub fn scope(user_id: Id, space_ids: Vec<Id>) -> VectorFilter {
        VectorFilter {
            scopes: vec![VectorScope { user_id, space_ids }],
            ..VectorFilter::default()
        }
    }

    /// Области, которые могут что-то пропустить (с непустым списком пространств).
    pub fn live_scopes(&self) -> impl Iterator<Item = &VectorScope> {
        self.scopes.iter().filter(|s| !s.space_ids.is_empty())
    }

    /// Фильтр не пропускает ни одной точки: нет области с пространствами.
    pub fn matches_nothing(&self) -> bool {
        self.live_scopes().next().is_none()
    }

    /// Фильтр prefetch папок: те же области и префикс пути, уровень и вид `folder`. Виды,
    /// статусы, `certainty`, `area` и виды инсайтов к папке не относятся (у её точки этих
    /// полей нет): по ним фильтруются поднимаемые папкой потомки.
    pub fn folder_filter(&self) -> VectorFilter {
        VectorFilter {
            scopes: self.scopes.clone(),
            path_prefix: self.path_prefix.clone(),
            levels: vec![LEVEL_FOLDER.to_string()],
            kinds: vec![LEVEL_FOLDER.to_string()],
            ..VectorFilter::default()
        }
    }
}

/// Результат поиска: в гибридном поиске score — RRF-оценка слияния, в
/// [`VectorIndex::nearest_dense`] — косинус.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorHit {
    pub chunk_id: String,
    /// Владелец базы, в которой лежит цель.
    pub user_id: Id,
    pub target: TargetRef,
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
    /// Создаёт коллекцию с метаданными `{model_id, schema}`, если её нет. В «своей»
    /// коллекции (тот же `model_id`) досоздаёт недостающие payload-индексы — так коллекция
    /// M2 получает индексы полей записей без пересборки.
    /// Существующую коллекцию с другим `model_id` или `dim` не трогает и возвращает её
    /// состояние: решение (выключить векторный канал до reindex) принимает вызывающий.
    fn ensure<'a>(&'a self, model_id: &'a str, dim: usize) -> IndexFuture<'a, IndexMeta>;

    /// Upsert точек пачками, `wait=true`. Точки пространства `human` и точки без ровно
    /// одной цели отклоняются целиком.
    fn upsert(&self, pts: Vec<IndexPoint>) -> IndexFuture<'_, ()>;

    /// Удаляет все точки узла пользователя (по фильтру `user_id` + `node_id`), `wait=true`.
    fn delete_node(&self, user: Id, node: Id) -> IndexFuture<'_, ()> {
        Box::pin(async move {
            let t = [TargetRef {
                kind: TargetKind::Node,
                id: node,
            }];
            self.delete_targets(user, &t).await
        })
    }

    /// Удаляет все точки целей `targets` тенанта `user` (узлы — по `node_id`, записи — по
    /// `entry_id`) одной операцией, `wait=true` (пачка заданий воркера). Пустой список —
    /// ничего не делает.
    fn delete_targets<'a>(&'a self, user: Id, targets: &'a [TargetRef]) -> IndexFuture<'a, ()>;

    /// Удаляет точки, которые пропускает фильтр (пустой `levels` — все уровни),
    /// `wait=true`: reindex области из кэша `embeddings`.
    fn delete_scope<'a>(&'a self, f: &'a VectorFilter) -> IndexFuture<'a, ()>;

    /// Гибридный поиск: prefetch dense и sparse по чанкам + dense по заметкам и секциям,
    /// слияние RRF. Фильтр — общий для всех prefetch.
    fn search<'a>(
        &'a self,
        q_dense: &'a [f32],
        q_sparse: &'a [(u32, f32)],
        f: &'a VectorFilter,
    ) -> IndexFuture<'a, Vec<VectorHit>>;

    /// Ближайшие по dense-вектору точки фильтра (пустой `levels` — все уровни), не больше
    /// `limit`, по убыванию; score — косинус (поиск по int8 с rescoring по оригиналам).
    /// Для поиска дубликатов инсайтов.
    fn nearest_dense<'a>(
        &'a self,
        v: &'a [f32],
        f: &'a VectorFilter,
        limit: usize,
    ) -> IndexFuture<'a, Vec<VectorHit>>;

    /// Точное число точек по фильтру; пустой `levels` — все уровни (для `/index/status`
    /// и проверок исключений).
    fn count<'a>(&'a self, f: &'a VectorFilter) -> IndexFuture<'a, u64>;

    /// Удаляет коллекцию целиком (reindex, смена модели). Идемпотентно.
    fn drop_all(&self) -> IndexFuture<'_, ()>;

    /// Проверка доступности и ключа (аутентифицированный запрос).
    fn health(&self) -> IndexFuture<'_, ()>;
}

/// Проверка точек перед upsert (общая для адаптеров): Human никогда не попадает в индекс
/// (§6.4), у точки есть dense-вектор и ровно одна цель.
pub(crate) fn check_points(pts: &[IndexPoint]) -> Result<(), IndexError> {
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
    if pts.iter().any(|p| p.payload.target().is_none()) {
        return Err(IndexError::Other(
            "у точки должна быть ровно одна цель: node_id или entry_id".into(),
        ));
    }
    Ok(())
}

/// Цели, разделённые по виду: (id узлов, id записей) в base32.
pub(crate) fn split_targets(targets: &[TargetRef]) -> (Vec<String>, Vec<String>) {
    let mut nodes = Vec::new();
    let mut entries = Vec::new();
    for t in targets {
        match t.kind {
            TargetKind::Node => nodes.push(t.id.to_string()),
            TargetKind::Entry => entries.push(t.id.to_string()),
        }
    }
    (nodes, entries)
}

/// Цель точки по строковым `node_id` / `entry_id` из payload выдачи.
pub(crate) fn hit_target(
    node_id: Option<String>,
    entry_id: Option<String>,
) -> Result<TargetRef, IndexError> {
    let parse = |s: String| {
        s.parse::<Id>()
            .map_err(|_| IndexError::Corrupt("точка с некорректным id цели".into()))
    };
    match (node_id, entry_id) {
        (Some(n), None) => Ok(TargetRef {
            kind: TargetKind::Node,
            id: parse(n)?,
        }),
        (None, Some(e)) => Ok(TargetRef {
            kind: TargetKind::Entry,
            id: parse(e)?,
        }),
        _ => Err(IndexError::Corrupt(
            "у точки нет ровно одной цели (node_id / entry_id)".into(),
        )),
    }
}

/// Уровень и вид точки папки.
pub const LEVEL_FOLDER: &str = "folder";

/// Поля payload, которые читает выдача поиска.
pub(crate) const HIT_FIELDS: [&str; 5] = ["chunk_id", "user_id", "node_id", "entry_id", "level"];

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

    #[test]
    fn filter_without_spaces_matches_nothing() {
        let u = Id::new();
        assert!(VectorFilter::default().matches_nothing());
        assert!(VectorFilter::scope(u, vec![]).matches_nothing());
        assert!(!VectorFilter::scope(u, vec![Id::new()]).matches_nothing());
    }

    #[test]
    fn folder_filter_keeps_scope_and_prefix_only() {
        let u = Id::new();
        let f = VectorFilter {
            path_prefix: Some("ops".into()),
            kinds: vec!["note".into()],
            certainty: vec!["axiom".into()],
            area: Some("db".into()),
            exclude_statuses: vec!["retracted".into()],
            folders: true,
            ..VectorFilter::scope(u, vec![Id::new()])
        };
        let g = f.folder_filter();
        assert_eq!(g.scopes, f.scopes);
        assert_eq!(g.path_prefix.as_deref(), Some("ops"));
        assert_eq!(
            (g.levels, g.kinds),
            (vec!["folder".to_string()], vec!["folder".to_string()])
        );
        assert!(g.certainty.is_empty() && g.area.is_none() && g.exclude_statuses.is_empty());
        assert!(!g.folders);
    }

    #[test]
    fn hit_target_needs_exactly_one() {
        let id = Id::new();
        assert_eq!(
            hit_target(Some(id.to_string()), None).unwrap(),
            TargetRef {
                kind: TargetKind::Node,
                id
            }
        );
        assert_eq!(
            hit_target(None, Some(id.to_string())).unwrap().kind,
            TargetKind::Entry
        );
        assert!(hit_target(None, None).is_err());
        assert!(hit_target(Some(id.to_string()), Some(id.to_string())).is_err());
        assert!(hit_target(Some("bad".into()), None).is_err());
    }
}
