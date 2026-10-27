//! Интеграционные тесты адаптера Qdrant на настоящем Qdrant 1.19.2 (задача 5 M2).
//! Общий контракт [`VectorIndex`] — в `common/cases.rs` (он же прогоняется для Edge);
//! здесь — его привязка к процессу Qdrant и проверки, специфичные для сервера.

mod common;

use std::sync::Arc;

use common::cases::{Ctx, DIM, MODEL, filter, point, sparse_for, vec_for};
use common::qdrant::TestQdrant;
use zam_core::Id;
use zam_index::{IndexError, QdrantIndex, VectorIndex, point_id};

fn connect(q: &TestQdrant) -> QdrantIndex {
    QdrantIndex::connect(&q.grpc_url(), Some(&q.api_key)).expect("connect")
}

/// Свежий Qdrant и клиент к нему. Трейт — как объект: так его держат супервизор и воркер.
fn managed(name: &str) -> (Arc<dyn VectorIndex>, TestQdrant) {
    let q = TestQdrant::start(name);
    (Arc::new(connect(&q)), q)
}

vector_index_cases!(managed);

/// Точный поиск (`QdrantIndex::with_exact_search`, `qdrant.exact_search` — прогоны гейта eval,
/// задача 15 M4): Qdrant принимает `exact` и `quantization.ignore` во всех prefetch (листы dense и
/// sparse, сводки, папки) и в поиске ближайших, находки — те же, что в контракте HNSW.
mod exact {
    use super::*;

    fn managed_exact(name: &str) -> (Arc<dyn VectorIndex>, TestQdrant) {
        let q = TestQdrant::start(name);
        (Arc::new(connect(&q).with_exact_search(true)), q)
    }

    vector_index_cases!(@each managed_exact;
        upsert_200_and_search_finds_itself_first,
        levels_filter_and_note_prefetch,
        folders_prefetch_on_request,
        nearest_dense_returns_cosine,
    );
}

#[test]
fn point_id_is_deterministic_uuid_v5() {
    let a = point_id("chunk-1");
    assert_eq!(a, point_id("chunk-1"));
    assert_ne!(a, point_id("chunk-2"));
    assert_eq!(a.get_version_num(), 5);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_api_key_is_auth_error() {
    let q = TestQdrant::start("auth");
    let idx = QdrantIndex::connect(&q.grpc_url(), Some("wrong-key")).unwrap();
    let err = idx.health().await.unwrap_err();
    assert!(matches!(err, IndexError::Auth), "{err:?}");
    let err = idx.ensure(MODEL, DIM).await.unwrap_err();
    assert!(matches!(err, IndexError::Auth), "{err:?}");
    // отправленный ключ не утекает в текст ошибки
    let shown = format!("{err} {err:?}");
    assert!(!shown.contains("wrong-key"), "{shown}");
    assert!(!shown.contains(&q.api_key), "{shown}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unreachable_server_is_unavailable() {
    // Порт без слушателя.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let idx = QdrantIndex::connect(&format!("http://127.0.0.1:{port}"), Some("k")).unwrap();
    let err = idx.health().await.unwrap_err();
    assert!(matches!(err, IndexError::Unavailable(_)), "{err:?}");
}

/// Qdrant падает посреди работы (процесс убит): все операции того же клиента — `Unavailable`
/// (а не паника или иная ошибка), после перезапуска клиент работает снова без пересоздания.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn killed_mid_work_is_unavailable_then_recovers() {
    let mut q = TestQdrant::start("kill");
    let idx = connect(&q);
    idx.ensure(MODEL, DIM).await.unwrap();
    let ctx = Ctx {
        user: Id::new(),
        space: Id::new(),
    };
    let node = Id::new();
    idx.upsert(vec![point(&ctx, node, "c1", 1, "a/b", "chunk")])
        .await
        .unwrap();
    q.stop();
    let unavailable = |what: &str, e: IndexError| {
        assert!(matches!(e, IndexError::Unavailable(_)), "{what}: {e:?}");
    };
    unavailable("health", idx.health().await.unwrap_err());
    unavailable(
        "search",
        idx.search(&vec_for(1), &sparse_for(1), &filter(&ctx))
            .await
            .unwrap_err(),
    );
    unavailable(
        "upsert",
        idx.upsert(vec![point(&ctx, node, "c2", 2, "a/b", "chunk")])
            .await
            .unwrap_err(),
    );
    unavailable("count", idx.count(&filter(&ctx)).await.unwrap_err());
    unavailable("ensure", idx.ensure(MODEL, DIM).await.unwrap_err());
    unavailable("delete", idx.delete_node(ctx.user, node).await.unwrap_err());

    q.restart();
    // тот же клиент переподключается сам
    let t = std::time::Instant::now();
    while idx.health().await.is_err() {
        assert!(
            t.elapsed() < std::time::Duration::from_secs(60),
            "клиент не переподключился"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let hits = idx
        .search(&vec_for(1), &sparse_for(1), &filter(&ctx))
        .await
        .unwrap();
    assert_eq!(hits[0].chunk_id, "c1");
    idx.upsert(vec![point(&ctx, node, "c2", 2, "a/b", "chunk")])
        .await
        .unwrap();
    assert_eq!(idx.count(&filter(&ctx)).await.unwrap(), 2);
}

#[test]
fn free_port_is_not_issued_twice_in_process() {
    let ports: Vec<u16> = (0..200).map(|_| common::qdrant::free_port()).collect();
    let unique: std::collections::HashSet<u16> = ports.iter().copied().collect();
    assert_eq!(unique.len(), ports.len());
}

/// Коллекция M2 (без payload-индексов записей журналов): `ensure` досоздаёт индексы
/// `entry_id`, `kind`, `status`, `insight_kind`, `certainty`, `area` на месте — коллекция
/// не пересоздаётся, точки остаются, `metadata.schema` по-прежнему 1.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ensure_adds_indexes_to_existing_collection() {
    use qdrant_client::Qdrant;
    use qdrant_client::qdrant::DeleteFieldIndexCollectionBuilder;

    const M3_FIELDS: [&str; 6] = [
        "entry_id",
        "kind",
        "status",
        "insight_kind",
        "certainty",
        "area",
    ];
    let q = TestQdrant::start("upgr");
    let idx = connect(&q);
    let raw = Qdrant::from_url(&q.grpc_url())
        .api_key(Some(q.api_key.clone()))
        .skip_compatibility_check()
        .build()
        .unwrap();
    assert!(idx.ensure(MODEL, DIM).await.unwrap().created);
    let ctx = Ctx {
        user: Id::new(),
        space: Id::new(),
    };
    idx.upsert(vec![
        point(&ctx, Id::new(), "m2-a", 1, "a", "chunk"),
        point(&ctx, Id::new(), "m2-b", 2, "a", "note"),
    ])
    .await
    .unwrap();
    // приводим коллекцию к схеме M2: индексов полей записей нет
    for field in M3_FIELDS {
        raw.delete_field_index(
            DeleteFieldIndexCollectionBuilder::new(zam_index::COLLECTION, field).wait(true),
        )
        .await
        .unwrap();
    }
    let schema = async || -> Vec<String> {
        let info = raw
            .collection_info(zam_index::COLLECTION)
            .await
            .unwrap()
            .result
            .unwrap();
        info.payload_schema.into_keys().collect()
    };
    let before = schema().await;
    assert!(
        M3_FIELDS.iter().all(|f| !before.iter().any(|s| s == f)),
        "{before:?}"
    );

    let m = idx.ensure(MODEL, DIM).await.unwrap();
    assert!(!m.created);
    assert_eq!(m.points, 2);
    let after = schema().await;
    for f in M3_FIELDS.iter().chain(&[
        "user_id",
        "space_id",
        "node_id",
        "path_ancestors",
        "level",
        "updated_at",
    ]) {
        assert!(after.iter().any(|s| s == f), "нет индекса {f}: {after:?}");
    }
    let info = raw
        .collection_info(zam_index::COLLECTION)
        .await
        .unwrap()
        .result
        .unwrap();
    let schema_v = info.config.unwrap().metadata.get("schema").cloned();
    assert_eq!(schema_v, Some(qdrant_client::qdrant::Value::from(1i64)));
    // точки на месте и находятся; фильтр статуса пропускает точки без status
    let mut f = filter(&ctx);
    f.exclude_statuses = vec!["superseded".into(), "retracted".into()];
    let hits = idx.search(&vec_for(1), &sparse_for(1), &f).await.unwrap();
    assert_eq!(hits.len(), 2);
    // повторный ensure — без изменений
    assert!(!idx.ensure(MODEL, DIM).await.unwrap().created);
}
