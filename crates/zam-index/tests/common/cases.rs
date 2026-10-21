//! Общий набор интеграционных тестов [`VectorIndex`] (задачи 5 и 16 M2): одни и те же
//! проверки контракта прогоняются для managed Qdrant (`tests/qdrant.rs`) и для прототипа
//! на Qdrant Edge (`tests/edge.rs`, feature `edge`). Каждый случай получает свежий пустой
//! индекс; тесты, специфичные для процесса Qdrant (ключ, недоступность, падение), остаются
//! в `tests/qdrant.rs`.

use zam_core::Id;
use zam_index::{IndexError, IndexPoint, PointPayload, VectorFilter, VectorIndex};

pub const DIM: usize = 16;
pub const MODEL: &str = "test/minilm@abc123";

/// Детерминированный псевдослучайный единичный вектор (xorshift).
pub fn vec_for(seed: u64) -> Vec<f32> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let v: Vec<f32> = (0..DIM)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            ((x % 2000) as f32 / 1000.0) - 1.0
        })
        .collect();
    let n = v.iter().map(|a| a * a).sum::<f32>().sqrt();
    v.into_iter().map(|a| a / n).collect()
}

pub fn sparse_for(seed: u64) -> Vec<(u32, f32)> {
    vec![((seed % 1000) as u32, 0.9), (1000 + (seed % 7) as u32, 0.3)]
}

/// Предки пути: "a/b/c" → ["a", "a/b", "a/b/c"].
pub fn ancestors(path: &str) -> Vec<String> {
    let parts: Vec<&str> = path.split('/').collect();
    (1..=parts.len()).map(|i| parts[..i].join("/")).collect()
}

pub struct Ctx {
    pub user: Id,
    pub space: Id,
}

pub fn point(
    ctx: &Ctx,
    node: Id,
    chunk_id: &str,
    seed: u64,
    path: &str,
    level: &str,
) -> IndexPoint {
    IndexPoint {
        chunk_id: chunk_id.to_string(),
        dense: vec_for(seed),
        sparse: sparse_for(seed),
        payload: PointPayload {
            user_id: ctx.user,
            space_id: ctx.space,
            space_kind: "knowledge".into(),
            solution_guid: None,
            node_id: node,
            path_ancestors: ancestors(path),
            area: None,
            level: level.into(),
            kind: "note".into(),
            tags: vec!["t".into()],
            updated_at: 1_760_000_000,
            rev: 1,
            text_hash: format!("h{seed}"),
        },
    }
}

pub fn filter(ctx: &Ctx) -> VectorFilter {
    VectorFilter {
        user_id: ctx.user,
        space_ids: vec![ctx.space],
        path_prefix: None,
        levels: vec![],
    }
}

pub async fn ensure_creates_collection_and_is_idempotent(idx: &dyn VectorIndex) {
    idx.health().await.expect("health");
    let m1 = idx.ensure(MODEL, DIM).await.expect("ensure 1");
    assert!(m1.created);
    assert_eq!(m1.model_id.as_deref(), Some(MODEL));
    assert_eq!(m1.dim, DIM);
    assert_eq!(m1.points, 0);
    let m2 = idx.ensure(MODEL, DIM).await.expect("ensure 2");
    assert!(!m2.created);
    assert_eq!(m2.model_id.as_deref(), Some(MODEL));
    assert_eq!(m2.dim, DIM);
}

pub async fn ensure_with_other_model_returns_existing_meta_untouched(idx: &dyn VectorIndex) {
    idx.ensure(MODEL, DIM).await.unwrap();
    let ctx = Ctx {
        user: Id::new(),
        space: Id::new(),
    };
    idx.upsert(vec![point(&ctx, Id::new(), "c1", 1, "a", "chunk")])
        .await
        .unwrap();
    let m = idx
        .ensure("other/model@zzz", 32)
        .await
        .expect("ensure other");
    assert!(!m.created);
    assert_eq!(m.model_id.as_deref(), Some(MODEL));
    assert_eq!(m.dim, DIM);
    assert_eq!(m.points, 1);
}

pub async fn upsert_200_and_search_finds_itself_first(idx: &dyn VectorIndex) {
    idx.ensure(MODEL, DIM).await.unwrap();
    let ctx = Ctx {
        user: Id::new(),
        space: Id::new(),
    };
    let node = Id::new();
    let pts: Vec<IndexPoint> = (0..200)
        .map(|i| point(&ctx, node, &format!("chunk-{i}"), i, "a/b", "chunk"))
        .collect();
    idx.upsert(pts).await.unwrap();
    assert_eq!(idx.ensure(MODEL, DIM).await.unwrap().points, 200);

    let hits = idx
        .search(&vec_for(77), &sparse_for(77), &filter(&ctx))
        .await
        .unwrap();
    assert!(!hits.is_empty());
    assert!(hits.len() <= 60);
    assert_eq!(hits[0].chunk_id, "chunk-77");
    assert_eq!(hits[0].node_id, node);
    assert_eq!(hits[0].level, "chunk");
}

pub async fn upsert_same_chunk_id_does_not_duplicate(idx: &dyn VectorIndex) {
    idx.ensure(MODEL, DIM).await.unwrap();
    let ctx = Ctx {
        user: Id::new(),
        space: Id::new(),
    };
    let node = Id::new();
    for round in 0..3 {
        idx.upsert(vec![
            point(&ctx, node, "c-a", round, "a", "chunk"),
            point(&ctx, node, "c-b", round + 10, "a", "chunk"),
        ])
        .await
        .unwrap();
    }
    assert_eq!(idx.ensure(MODEL, DIM).await.unwrap().points, 2);
}

pub async fn filters_isolate_users_and_spaces(idx: &dyn VectorIndex) {
    idx.ensure(MODEL, DIM).await.unwrap();
    let a = Ctx {
        user: Id::new(),
        space: Id::new(),
    };
    let b = Ctx {
        user: Id::new(),
        space: Id::new(),
    };
    let a2 = Ctx {
        user: a.user,
        space: Id::new(),
    };
    let (na, nb, na2) = (Id::new(), Id::new(), Id::new());
    // Одинаковый вектор у всех: результат определяет только фильтр.
    idx.upsert(vec![
        point(&a, na, "a-1", 5, "x", "chunk"),
        point(&b, nb, "b-1", 5, "x", "chunk"),
        point(&a2, na2, "a2-1", 5, "x", "chunk"),
    ])
    .await
    .unwrap();

    let hits = idx
        .search(&vec_for(5), &sparse_for(5), &filter(&a))
        .await
        .unwrap();
    assert_eq!(
        hits.iter().map(|h| h.chunk_id.as_str()).collect::<Vec<_>>(),
        ["a-1"]
    );

    let hits = idx
        .search(&vec_for(5), &sparse_for(5), &filter(&b))
        .await
        .unwrap();
    assert_eq!(
        hits.iter().map(|h| h.chunk_id.as_str()).collect::<Vec<_>>(),
        ["b-1"]
    );

    // Оба пространства пользователя A — match any; чужой пользователь не виден.
    let mut f = filter(&a);
    f.space_ids = vec![a.space, a2.space, b.space];
    let mut ids: Vec<String> = idx
        .search(&vec_for(5), &sparse_for(5), &f)
        .await
        .unwrap()
        .into_iter()
        .map(|h| h.chunk_id)
        .collect();
    ids.sort();
    assert_eq!(ids, ["a-1", "a2-1"]);
}

pub async fn path_prefix_uses_path_ancestors(idx: &dyn VectorIndex) {
    idx.ensure(MODEL, DIM).await.unwrap();
    let ctx = Ctx {
        user: Id::new(),
        space: Id::new(),
    };
    idx.upsert(vec![
        point(&ctx, Id::new(), "p1", 1, "proj/docs/a", "chunk"),
        point(&ctx, Id::new(), "p2", 2, "proj/docs", "chunk"),
        point(&ctx, Id::new(), "p3", 3, "proj/src/main", "chunk"),
        // пробел в пути: должен оставаться keyword, а не полнотекстовым match
        point(&ctx, Id::new(), "p4", 4, "my notes/x", "chunk"),
        // «proj/doc» — префикс строки, но не предок пути
        point(&ctx, Id::new(), "p5", 5, "proj/doc", "chunk"),
    ])
    .await
    .unwrap();

    let mut f = filter(&ctx);
    f.path_prefix = Some("proj/docs".into());
    let mut ids: Vec<String> = idx
        .search(&vec_for(1), &sparse_for(1), &f)
        .await
        .unwrap()
        .into_iter()
        .map(|h| h.chunk_id)
        .collect();
    ids.sort();
    assert_eq!(ids, ["p1", "p2"]);

    f.path_prefix = Some("my notes".into());
    let ids: Vec<String> = idx
        .search(&vec_for(4), &sparse_for(4), &f)
        .await
        .unwrap()
        .into_iter()
        .map(|h| h.chunk_id)
        .collect();
    assert_eq!(ids, ["p4"]);
}

pub async fn levels_filter_and_note_prefetch(idx: &dyn VectorIndex) {
    idx.ensure(MODEL, DIM).await.unwrap();
    let ctx = Ctx {
        user: Id::new(),
        space: Id::new(),
    };
    let node = Id::new();
    idx.upsert(vec![
        point(&ctx, node, "n-note", 9, "a", "note"),
        point(&ctx, node, "n-sec", 9, "a", "section"),
        point(&ctx, node, "n-chunk", 9, "a", "chunk"),
    ])
    .await
    .unwrap();

    // Без ограничения уровней: chunk через dense+sparse, note/section через третий prefetch.
    let mut ids: Vec<String> = idx
        .search(&vec_for(9), &sparse_for(9), &filter(&ctx))
        .await
        .unwrap()
        .into_iter()
        .map(|h| h.chunk_id)
        .collect();
    ids.sort();
    assert_eq!(ids, ["n-chunk", "n-note", "n-sec"]);

    let mut f = filter(&ctx);
    f.levels = vec!["note".into()];
    let hits = idx.search(&vec_for(9), &sparse_for(9), &f).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].chunk_id, "n-note");
    assert_eq!(hits[0].level, "note");
}

pub async fn delete_node_removes_all_its_points(idx: &dyn VectorIndex) {
    idx.ensure(MODEL, DIM).await.unwrap();
    let ctx = Ctx {
        user: Id::new(),
        space: Id::new(),
    };
    let (n1, n2) = (Id::new(), Id::new());
    let mut pts: Vec<IndexPoint> = (0..5)
        .map(|i| point(&ctx, n1, &format!("n1-{i}"), i, "a", "chunk"))
        .collect();
    pts.push(point(&ctx, n2, "n2-0", 100, "a", "chunk"));
    idx.upsert(pts).await.unwrap();
    // Чужой пользователь с тем же node_id не удаляет чужие точки.
    idx.delete_node(Id::new(), n1).await.unwrap();
    assert_eq!(idx.ensure(MODEL, DIM).await.unwrap().points, 6);

    idx.delete_node(ctx.user, n1).await.unwrap();
    assert_eq!(idx.ensure(MODEL, DIM).await.unwrap().points, 1);
    let hits = idx
        .search(&vec_for(2), &sparse_for(2), &filter(&ctx))
        .await
        .unwrap();
    assert!(hits.iter().all(|h| h.node_id == n2));
}

pub async fn missing_collection_is_not_found_and_count_filters(idx: &dyn VectorIndex) {
    let ctx = Ctx {
        user: Id::new(),
        space: Id::new(),
    };
    // Коллекции нет: upsert и delete — NotFound (воркер вызывает ensure и повторяет).
    let err = idx
        .upsert(vec![point(&ctx, Id::new(), "c1", 1, "a", "chunk")])
        .await
        .unwrap_err();
    assert!(matches!(err, IndexError::NotFound(_)), "{err:?}");
    let err = idx.delete_node(ctx.user, Id::new()).await.unwrap_err();
    assert!(matches!(err, IndexError::NotFound(_)), "{err:?}");

    idx.ensure(MODEL, DIM).await.unwrap();
    let other = Ctx {
        user: ctx.user,
        space: Id::new(),
    };
    idx.upsert(vec![
        point(&ctx, Id::new(), "c1", 1, "a/b", "chunk"),
        point(&ctx, Id::new(), "c2", 2, "a/b", "note"),
        point(&other, Id::new(), "c3", 3, "x", "chunk"),
    ])
    .await
    .unwrap();
    assert_eq!(idx.count(&filter(&ctx)).await.unwrap(), 2);
    let mut f = filter(&ctx);
    f.levels = vec!["note".into()];
    assert_eq!(idx.count(&f).await.unwrap(), 1);
    f = filter(&other);
    f.path_prefix = Some("a".into());
    assert_eq!(idx.count(&f).await.unwrap(), 0);
    f.space_ids = vec![];
    assert_eq!(idx.count(&f).await.unwrap(), 2);
    f.user_id = Id::new();
    assert_eq!(idx.count(&f).await.unwrap(), 0);
}

pub async fn human_space_is_rejected(idx: &dyn VectorIndex) {
    idx.ensure(MODEL, DIM).await.unwrap();
    let ctx = Ctx {
        user: Id::new(),
        space: Id::new(),
    };
    let mut p = point(&ctx, Id::new(), "h1", 1, "a", "chunk");
    p.payload.space_kind = "Human".into();
    let err = idx.upsert(vec![p]).await.unwrap_err();
    assert!(matches!(err, IndexError::Other(_)), "{err:?}");
    assert_eq!(idx.ensure(MODEL, DIM).await.unwrap().points, 0);
}

pub async fn drop_all_removes_collection(idx: &dyn VectorIndex) {
    idx.ensure(MODEL, DIM).await.unwrap();
    idx.drop_all().await.unwrap();
    // повторно — без ошибки
    idx.drop_all().await.unwrap();
    let m = idx.ensure("new/model@1", 8).await.unwrap();
    assert!(m.created);
    assert_eq!(m.model_id.as_deref(), Some("new/model@1"));
    assert_eq!(m.dim, 8);
}

/// Тест на каждый общий случай: `$make(короткое имя)` возвращает свежий индекс и охрану его
/// окружения (процесс Qdrant или каталог Edge), которая живёт до конца теста.
#[macro_export]
macro_rules! vector_index_cases {
    ($make:path) => {
        $crate::vector_index_cases!(@each $make;
            ensure_creates_collection_and_is_idempotent,
            ensure_with_other_model_returns_existing_meta_untouched,
            upsert_200_and_search_finds_itself_first,
            upsert_same_chunk_id_does_not_duplicate,
            filters_isolate_users_and_spaces,
            path_prefix_uses_path_ancestors,
            levels_filter_and_note_prefetch,
            delete_node_removes_all_its_points,
            missing_collection_is_not_found_and_count_filters,
            human_space_is_rejected,
            drop_all_removes_collection,
        );
    };
    (@each $make:path; $($name:ident,)*) => {
        $(
            #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
            async fn $name() {
                // Короткое имя каталога: файлы сегментов Qdrant лежат глубоко, и длинное
                // имя теста выводит путь за MAX_PATH (260) на Windows.
                let name = stringify!($name);
                let (idx, _env) = $make(&name[..name.len().min(8)]);
                common::cases::$name(&*idx).await;
            }
        )*
    };
}
