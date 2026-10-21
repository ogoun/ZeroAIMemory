//! Интеграционные тесты прототипа [`EdgeIndex`] на Qdrant Edge (задача 16 M2, ADR 0001):
//! общий контракт [`VectorIndex`] из `common/cases.rs` (тот же, что у managed Qdrant) и
//! проверки, специфичные для встроенного индекса. Только с feature `edge`.
#![cfg(feature = "edge")]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use common::cases::{Ctx, DIM, MODEL, filter, point, sparse_for, vec_for};
use zam_core::Id;
use zam_index::{EdgeIndex, EdgeOptions, IndexPoint, VectorIndex};

/// Каталог теста под `CARGO_TARGET_TMPDIR`; удаляется в `Drop` (после закрытия индекса).
struct TestDir(PathBuf);

impl TestDir {
    fn new(name: &str) -> TestDir {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join("edge")
            .join(format!(
                "{name}-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("каталог теста");
        TestDir(dir)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn opts() -> EdgeOptions {
    EdgeOptions {
        optimize_debounce: Duration::from_millis(200),
        ..EdgeOptions::default()
    }
}

/// Свежий Edge-индекс в своём каталоге. Порядок в кортеже важен: индекс закрывается раньше,
/// чем удаляется каталог.
fn edge(name: &str) -> (Arc<dyn VectorIndex>, TestDir) {
    let dir = TestDir::new(name);
    let idx = EdgeIndex::open(&dir.0, opts()).expect("open");
    (Arc::new(idx), dir)
}

vector_index_cases!(edge);

fn ctx() -> Ctx {
    Ctx {
        user: Id::new(),
        space: Id::new(),
    }
}

/// Шард и `zam_meta.json` переживают закрытие: повторное открытие видит ту же модель и точки.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reopen_keeps_points_and_meta() {
    let dir = TestDir::new("reopen");
    let c = ctx();
    let node = Id::new();
    {
        let idx = EdgeIndex::open(&dir.0, opts()).unwrap();
        assert!(idx.ensure(MODEL, DIM).await.unwrap().created);
        let pts: Vec<IndexPoint> = (0..50)
            .map(|i| point(&c, node, &format!("c-{i}"), i, "a/b", "chunk"))
            .collect();
        idx.upsert(pts).await.unwrap();
    }
    let idx = EdgeIndex::open(&dir.0, opts()).unwrap();
    // до ensure шард уже открыт: поиск работает сразу
    let hits = idx
        .search(&vec_for(7), &sparse_for(7), &filter(&c))
        .await
        .unwrap();
    assert_eq!(hits[0].chunk_id, "c-7");
    let m = idx.ensure(MODEL, DIM).await.unwrap();
    assert!(!m.created);
    assert_eq!(m.model_id.as_deref(), Some(MODEL));
    assert_eq!(m.dim, DIM);
    assert_eq!(m.points, 50);
}

/// Явная оптимизация (слияние сегментов, HNSW) не теряет точек и не ломает поиск и фильтры.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn optimize_keeps_search_and_counts() {
    let (dir, c, node) = (TestDir::new("optimize"), ctx(), Id::new());
    let idx = EdgeIndex::open(&dir.0, opts()).unwrap();
    idx.ensure(MODEL, DIM).await.unwrap();
    // несколько пачек — несколько операций WAL и записей в appendable-сегмент
    for b in 0..4u64 {
        let pts: Vec<IndexPoint> = (b * 300..(b + 1) * 300)
            .map(|i| point(&c, node, &format!("c-{i}"), i, "x", "chunk"))
            .collect();
        idx.upsert(pts).await.unwrap();
    }
    idx.optimize_now().await.unwrap();
    let (segments, _) = idx.segments().await.unwrap();
    assert!(segments >= 1);
    assert_eq!(idx.count(&filter(&c)).await.unwrap(), 1200);
    let hits = idx
        .search(&vec_for(901), &sparse_for(901), &filter(&c))
        .await
        .unwrap();
    assert_eq!(hits[0].chunk_id, "c-901");
    idx.delete_node(c.user, node).await.unwrap();
    assert_eq!(idx.count(&filter(&c)).await.unwrap(), 0);
}

/// Фоновый оптимизатор после паузы в записи не мешает дальнейшей работе, а `drop_all`
/// дожидается освобождения шарда и удаляет его каталог.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_optimizer_and_drop_all() {
    let (dir, c, node) = (TestDir::new("background"), ctx(), Id::new());
    let idx = EdgeIndex::open(&dir.0, opts()).unwrap();
    idx.ensure(MODEL, DIM).await.unwrap();
    let pts: Vec<IndexPoint> = (0..500)
        .map(|i| point(&c, node, &format!("c-{i}"), i, "x", "chunk"))
        .collect();
    idx.upsert(pts).await.unwrap();
    // за паузу дебаунса оптимизатор успевает стартовать
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(idx.count(&filter(&c)).await.unwrap(), 500);
    idx.drop_all().await.unwrap();
    assert!(!dir.0.join("zam_chunks").exists());
    let m = idx.ensure(MODEL, DIM).await.unwrap();
    assert!(m.created);
    assert_eq!(m.points, 0);
}

/// Переменная окружения дочернего процесса теста устойчивости: каталог индекса.
const CHILD_DIR: &str = "ZAM_EDGE_CHILD_DIR";

/// Дочерняя половина [`confirmed_upsert_survives_process_exit`]: без переменной окружения
/// ничего не делает. С ней — upsert 100 точек (подтверждён: `Ok`) и выход процесса без
/// `Drop` (как убийство: `flush` в `Drop` шарда не выполняется).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn child_upsert_then_exit() {
    let Ok(dir) = std::env::var(CHILD_DIR) else {
        return;
    };
    let idx = EdgeIndex::open(Path::new(&dir), opts()).unwrap();
    idx.ensure(MODEL, DIM).await.unwrap();
    let (c, node) = (ctx(), Id::new());
    let pts: Vec<IndexPoint> = (0..100)
        .map(|i| point(&c, node, &format!("c-{i}"), i, "a", "chunk"))
        .collect();
    idx.upsert(pts).await.unwrap();
    std::process::exit(0);
}

/// Подтверждённый upsert переживает завершение процесса без `flush` (убийство, сбой).
/// Edge 0.8 этого не обеспечивает: `EdgeShard::load` не воспроизводит WAL
/// (`src/edge/edge_shard/mod.rs`, вызова `SerdeWal::read_all` нет нигде в крейте), а
/// сегменты сбрасываются на диск только в `flush`. Тест фиксирует дефект (ADR 0001,
/// «Зрелость API»); ожидаемо падает — запуск: `--features edge -- --ignored`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "известный дефект qdrant-edge 0.8: WAL не воспроизводится при load (ADR 0001)"]
async fn confirmed_upsert_survives_process_exit() {
    let dir = TestDir::new("crash");
    let st = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["child_upsert_then_exit", "--exact", "--nocapture"])
        .env(CHILD_DIR, &dir.0)
        .status()
        .unwrap();
    assert!(st.success(), "дочерний процесс: {st:?}");
    let idx = EdgeIndex::open(&dir.0, opts()).unwrap();
    let m = idx.ensure(MODEL, DIM).await.unwrap();
    // коллекция дочернего процесса найдена, а не создана заново (иначе 0 точек — по другой причине)
    assert!(!m.created, "коллекция создана заново: {m:?}");
    assert_eq!(
        m.points, 100,
        "подтверждённые точки потеряны после выхода без flush"
    );
}
