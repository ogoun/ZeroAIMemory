//! Воркер индексации на настоящем Qdrant 1.19.2 и детерминированной тестовой модели
//! (без ONNX Runtime): outbox → кэш → upsert, исключения, смена модели, потеря коллекции,
//! недоступность Qdrant, L0 и свёртка папок.

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use common::qdrant::TestQdrant;
use zam_core::Id;
use zam_index::{
    PassageEmbedder, QdrantIndex, VectorFilter, VectorIndex, WorkerConfig, WorkerDeps, WorkerHandle,
};
use zam_store::{
    Attribution, AuthorKind, DataDir, Db, Embedding, NewNote, NodePath, SpaceKind, SystemStore,
    UserDbs, chunks_for_index, create_note, delete_node, get_node, job_stats, list_spaces,
    update_note,
};

const DIM: usize = 16;
const MODEL: &str = "fake/test@00000001";
/// Предел ожиданий: создание коллекции с payload-индексами на HDD занимает секунды.
const LIMIT: Duration = Duration::from_secs(60);

/// Детерминированная «модель»: вектор из blake3 текста, считает вызовы.
struct Fake {
    texts: AtomicU64,
}

impl PassageEmbedder for Fake {
    fn model_id(&self) -> &str {
        MODEL
    }
    fn dim(&self) -> usize {
        DIM
    }
    fn embed_passages(&self, texts: &[&str]) -> Result<Vec<Embedding>, String> {
        self.texts.fetch_add(texts.len() as u64, Ordering::SeqCst);
        Ok(texts
            .iter()
            .map(|t| {
                let h = blake3::hash(t.as_bytes());
                let dense: Vec<f32> = h.as_bytes()[..DIM]
                    .iter()
                    .map(|b| *b as f32 / 255.0 - 0.5)
                    .collect();
                Embedding {
                    dense,
                    sparse: vec![(u32::from(h.as_bytes()[0]), 0.5)],
                }
            })
            .collect())
    }
    fn count_tokens(&self, text: &str) -> usize {
        text.split_whitespace().count()
    }
}

struct Env {
    _q: TestQdrant,
    vector: Arc<dyn VectorIndex>,
    users: UserDbs,
    system: SystemStore,
    user: Id,
    db: Db,
    knowledge: Id,
    human: Id,
    fake: Arc<Fake>,
}

async fn env(name: &str) -> Env {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("zam_index=debug")
        .with_test_writer()
        .try_init();
    let q = TestQdrant::start(&format!("worker-{name}"));
    let vector: Arc<dyn VectorIndex> =
        Arc::new(QdrantIndex::connect(&q.grpc_url(), Some(&q.api_key)).unwrap());
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("zam-index-worker")
        .join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let dd = DataDir::open(&dir).unwrap();
    let system = SystemStore::open(&dd).unwrap();
    let user = system.create_user("u", false).await.unwrap().id;
    let users = UserDbs::new(dd.users_dir());
    let db = users.get(user).await.unwrap();
    let spaces = list_spaces(&db).await.unwrap();
    let find = |k: SpaceKind| spaces.iter().find(|s| s.kind == k).unwrap().id;
    // каталог данных держит лок: живёт до конца процесса теста
    std::mem::forget(dd);
    Env {
        _q: q,
        vector,
        users,
        system,
        user,
        knowledge: find(SpaceKind::Knowledge),
        human: find(SpaceKind::Human),
        db,
        fake: Arc::new(Fake {
            texts: AtomicU64::new(0),
        }),
    }
}

impl Env {
    fn spawn(&self) -> WorkerHandle {
        self.spawn_with(self.vector.clone())
    }

    fn spawn_with(&self, vector: Arc<dyn VectorIndex>) -> WorkerHandle {
        WorkerHandle::spawn(
            WorkerDeps {
                system: self.system.clone(),
                users: self.users.clone(),
                embedder: self.fake.clone(),
                vector,
                supervisor: None,
            },
            WorkerConfig {
                rollup_debounce: Duration::from_millis(200),
                idle_poll: Duration::from_millis(200),
                retry_max: Duration::from_millis(500),
                ..WorkerConfig::default()
            },
        )
    }

    async fn note(&self, space: Id, path: &str, content: &str) -> Id {
        create_note(
            &self.db,
            NewNote {
                space_id: space,
                path: NodePath::parse(path).unwrap(),
                content: content.into(),
                l0: None,
                attrib: Attribution::new(AuthorKind::Agent),
            },
        )
        .await
        .unwrap()
        .id
    }

    async fn count(&self, space: Option<Id>, prefix: Option<&str>, levels: &[&str]) -> u64 {
        let f = VectorFilter {
            user_id: self.user,
            space_ids: space.into_iter().collect(),
            path_prefix: prefix.map(str::to_string),
            levels: levels.iter().map(|l| l.to_string()).collect(),
        };
        self.vector.count(&f).await.unwrap()
    }

    async fn state(&self, node: Id) -> String {
        get_node(&self.db, node).await.unwrap().index_state
    }

    /// Ждёт `index_state = 'indexed'` (до 60 с: создание коллекции на HDD — секунды).
    async fn indexed(&self, node: Id) {
        let t = Instant::now();
        while self.state(node).await != "indexed" {
            assert!(t.elapsed() < LIMIT, "узел не проиндексирован");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn until(&self, what: &str, f: impl AsyncFn() -> bool) {
        let t = Instant::now();
        while !f().await {
            assert!(t.elapsed() < LIMIT, "не дождались: {what}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

const NOTE: &str = "# Сборка\n\nСобираем проект командой cargo build в корне. Тесты гоняем отдельно командой cargo test.\n\n## Окружение\n\nПеременные окружения задаёт скрипт dev-env перед сборкой.\n";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn indexes_replaces_and_deletes_points_and_skips_excluded() {
    let e = env("flow").await;
    let a = e.note(e.knowledge, "docs/a", NOTE).await;
    let human = e.note(e.human, "h", NOTE).await;
    let off = e
        .note(
            e.knowledge,
            "off",
            &format!("---\nindex: false\n---\n{NOTE}"),
        )
        .await;
    let w = e.spawn();
    e.indexed(a).await;
    let chunks = chunks_for_index(&e.db, a).await.unwrap().len() as u64;
    assert!(chunks >= 2);
    assert_eq!(e.count(None, Some("docs/a"), &[]).await, chunks);
    // Human и index: false в Qdrant не попадают
    assert_eq!(e.count(Some(e.human), None, &[]).await, 0);
    assert_eq!(e.count(None, Some("off"), &[]).await, 0);
    assert_eq!(
        (e.state(human).await, e.state(off).await),
        ("excluded".into(), "excluded".into())
    );

    // тот же текст в другой заметке: модель не вызывается (кэш по text_hash)
    let before = e.fake.texts.load(Ordering::SeqCst);
    let b = e.note(e.knowledge, "docs/b", NOTE).await;
    w.notify();
    e.indexed(b).await;
    assert_eq!(e.fake.texts.load(Ordering::SeqCst), before);
    assert_eq!(w.embedded_texts(), before);

    // правка заменяет точки: старых chunk_id нет, число = текущие чанки
    update_note(
        &e.db,
        a,
        1,
        "Совсем другой текст про релизы и теги.\n".into(),
        None,
        Attribution::new(AuthorKind::Agent),
    )
    .await
    .unwrap();
    w.notify();
    e.until("точки правки", async || {
        e.state(a).await == "indexed"
            && e.count(None, Some("docs/a"), &[]).await
                == chunks_for_index(&e.db, a).await.unwrap().len() as u64
    })
    .await;
    assert!(e.count(None, Some("docs/a"), &[]).await < chunks);

    // удаление убирает точки
    delete_node(&e.db, a).await.unwrap();
    w.notify();
    e.until("удаление точек", async || {
        e.count(None, Some("docs/a"), &["chunk", "note", "section"])
            .await
            == 0
    })
    .await;
    assert_eq!(job_stats(&e.db).await.unwrap().queued, 0);
    w.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foreign_model_collection_is_not_written() {
    let e = env("mismatch").await;
    e.vector.ensure("other/model@ffff", DIM).await.unwrap();
    let a = e.note(e.knowledge, "a", NOTE).await;
    let w = e.spawn();
    let t = Instant::now();
    while w.status().model_mismatch.is_none() {
        assert!(t.elapsed() < LIMIT, "mismatch не обнаружен");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let st = w.status();
    assert_eq!(
        st.model_mismatch,
        Some(("other/model@ffff".to_string(), MODEL.to_string()))
    );
    assert!(!st.vector_ready);
    w.notify();
    tokio::time::sleep(Duration::from_millis(800)).await;
    // ни одной точки в чужой коллекции, задание ждёт, модель не вызывалась
    assert_eq!(e.vector.ensure(MODEL, DIM).await.unwrap().points, 0);
    assert_eq!(e.state(a).await, "pending");
    assert_eq!(job_stats(&e.db).await.unwrap().queued, 1);
    assert_eq!(w.embedded_texts(), 0);
    w.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropped_collection_is_rebuilt_from_embeddings() {
    let e = env("dropped").await;
    let a = e.note(e.knowledge, "a", NOTE).await;
    let w = e.spawn();
    e.indexed(a).await;
    let before = w.embedded_texts();
    let points = e.count(None, None, &[]).await;
    e.vector.drop_all().await.unwrap();
    // новая запись: upsert получает NotFound → ensure создаёт коллекцию → reindex
    let b = e
        .note(e.knowledge, "b", "Отдельная заметка о деплое.\n")
        .await;
    w.notify();
    e.indexed(b).await;
    e.indexed(a).await;
    e.until("точки восстановлены", async || {
        e.count(None, Some("a"), &[]).await
            == chunks_for_index(&e.db, a).await.unwrap().len() as u64
    })
    .await;
    assert!(e.count(None, None, &[]).await >= points);
    // инференс только для нового текста
    assert!(
        w.embedded_texts() - before <= 2,
        "{}",
        w.embedded_texts() - before
    );
    w.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unavailable_qdrant_keeps_queue_and_catches_up() {
    let mut e = env("outage").await;
    let w = e.spawn();
    let a = e.note(e.knowledge, "a", NOTE).await;
    w.notify();
    e.indexed(a).await;

    e._q.stop();
    let b = e
        .note(e.knowledge, "b", "Заметка во время простоя Qdrant.\n")
        .await;
    w.notify();
    let t = Instant::now();
    while w.status().vector_ready {
        assert!(t.elapsed() < LIMIT, "простой не замечен");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(e.state(b).await, "pending");
    let st = job_stats(&e.db).await.unwrap();
    assert_eq!((st.queued, st.failed), (1, 0));
    // попытки не сгорели
    let attempts: i64 =
        e.db.read(|c| Ok(c.query_row("SELECT max(attempts) FROM index_jobs", [], |r| r.get(0))?))
            .await
            .unwrap();
    assert_eq!(attempts, 0);

    e._q.restart();
    e.indexed(b).await;
    assert!(w.status().vector_ready);
    w.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_l0_and_folder_rollup() {
    let e = env("rollup").await;
    let n = e
        .note(
            e.knowledge,
            "proj/docs/build",
            "Первый абзац про сборку проекта.\n\n## Детали\n\nСборка идёт командой cargo build в корне репозитория. \
             Тесты запускаются отдельно и долго. Переменные окружения задаёт скрипт.\n",
        )
        .await;
    let manual = e
        .note(
            e.knowledge,
            "proj/manual",
            "---\nl0: Ручная сводка\n---\nТекст заметки.\n",
        )
        .await;
    let w = e.spawn();
    e.indexed(n).await;
    e.indexed(manual).await;
    let node = get_node(&e.db, n).await.unwrap();
    let l0 = node.l0.clone().unwrap();
    assert_eq!(node.l0_source.as_deref(), Some("auto"));
    assert!(l0.split_whitespace().count() <= 50);
    assert_eq!(
        get_node(&e.db, manual).await.unwrap().l0.as_deref(),
        Some("Ручная сводка")
    );

    let docs = get_node(&e.db, node.parent_id.unwrap()).await.unwrap();
    let proj = docs.parent_id.unwrap();
    e.until("свёртка папок", async || {
        get_node(&e.db, proj)
            .await
            .unwrap()
            .l0
            .is_some_and(|l| l.contains("docs/:") && l.contains("manual: Ручная сводка"))
    })
    .await;
    let docs_l0 = get_node(&e.db, docs.id).await.unwrap().l0.unwrap();
    assert_eq!(docs_l0, format!("build: {l0}"));
    e.until("точки папок", async || {
        e.count(None, Some("proj"), &["folder"]).await == 2
    })
    .await;
    w.shutdown().await;
}
