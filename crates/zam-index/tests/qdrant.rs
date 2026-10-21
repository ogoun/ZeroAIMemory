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
