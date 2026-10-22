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
    PassageEmbedder, QdrantIndex, ReindexRequest, VectorFilter, VectorIndex, WorkerConfig,
    WorkerDeps, WorkerHandle, embed_key,
};
use zam_store::{
    Attribution, AuthorKind, DataDir, Db, Embedding, NewNote, NodePath, SpaceKind, SystemStore,
    UserDbs, chunks_for_index, create_note, delete_node, enqueue_reindex, get_embeddings, get_node,
    job_stats, list_spaces, mark_reindex, move_node, pending_reindex, put_embeddings, update_note,
};

const DIM: usize = 16;
const MODEL: &str = "fake/test@00000001";
/// Предел ожиданий: создание коллекции с payload-индексами на HDD занимает секунды, а при
/// параллельном прогоне тестов (до 7 процессов Qdrant) — до минуты и больше.
const LIMIT: Duration = Duration::from_secs(240);

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

/// Все пространства базы.
async fn all_spaces(db: &Db) -> Vec<Id> {
    list_spaces(db)
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect()
}

struct Env {
    _q: TestQdrant,
    users_dir: PathBuf,
    vector: Arc<dyn VectorIndex>,
    users: UserDbs,
    system: SystemStore,
    user: Id,
    db: Db,
    knowledge: Id,
    rules: Id,
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
    let users_dir = dd.users_dir();
    let users = UserDbs::new(users_dir.clone());
    let db = users.get(user).await.unwrap();
    let spaces = list_spaces(&db).await.unwrap();
    let find = |k: SpaceKind| spaces.iter().find(|s| s.kind == k).unwrap().id;
    // каталог данных держит лок: живёт до конца процесса теста
    std::mem::forget(dd);
    Env {
        _q: q,
        users_dir,
        vector,
        users,
        system,
        user,
        knowledge: find(SpaceKind::Knowledge),
        rules: find(SpaceKind::Rules),
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
        self.spawn_gc(vector, Duration::from_millis(300))
    }

    /// Воркер с заданным периодом сборки мусора кэша.
    fn spawn_gc(&self, vector: Arc<dyn VectorIndex>, gc_interval: Duration) -> WorkerHandle {
        self.spawn_full(self.users.clone(), vector, gc_interval)
    }

    /// Воркер со своим кэшем баз (файлы баз закрываются вместе с ним).
    fn spawn_on(&self, users: UserDbs) -> WorkerHandle {
        self.spawn_full(users, self.vector.clone(), Duration::from_millis(300))
    }

    fn spawn_full(
        &self,
        users: UserDbs,
        vector: Arc<dyn VectorIndex>,
        gc_interval: Duration,
    ) -> WorkerHandle {
        WorkerHandle::spawn(
            WorkerDeps {
                system: self.system.clone(),
                users,
                embedder: self.fake.clone(),
                vector,
                supervisor: None,
            },
            WorkerConfig {
                rollup_debounce: Duration::from_millis(200),
                idle_poll: Duration::from_millis(200),
                retry_max: Duration::from_millis(500),
                gc_interval,
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
        let spaces = match space {
            Some(s) => vec![s],
            None => all_spaces(&self.db).await,
        };
        let f = VectorFilter {
            path_prefix: prefix.map(str::to_string),
            levels: levels.iter().map(|l| l.to_string()).collect(),
            ..VectorFilter::scope(self.user, spaces)
        };
        self.vector.count(&f).await.unwrap()
    }

    /// Фильтр всей базы пользователя теста (все её пространства).
    async fn whole_base(&self) -> VectorFilter {
        VectorFilter::scope(self.user, all_spaces(&self.db).await)
    }

    async fn state(&self, node: Id) -> String {
        get_node(&self.db, node).await.unwrap().index_state
    }

    /// Ждёт `index_state = 'indexed'` (до [`LIMIT`]: создание коллекции на HDD).
    async fn indexed(&self, node: Id) {
        let t = Instant::now();
        while self.state(node).await != "indexed" {
            assert!(t.elapsed() < LIMIT, "узел не проиндексирован");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Чанков узла (= точек его заметки в индексе).
    async fn chunks(&self, node: Id) -> u64 {
        chunks_for_index(&self.db, node).await.unwrap().len() as u64
    }

    /// Строк кэша эмбеддингов.
    async fn cached(&self) -> i64 {
        self.db
            .read(|c| Ok(c.query_row("SELECT count(*) FROM embeddings", [], |r| r.get(0))?))
            .await
            .unwrap()
    }

    /// Ждёт покоя воркера: заданий нет (ни в очереди, ни в работе), пачка (с auto-L0 после
    /// закрытия заданий) не обрабатывается и свёрток L0 не ждёт ни одна папка — после
    /// этого модель без новых записей не вызывается.
    async fn quiet(&self, w: &WorkerHandle) {
        self.until("воркер в покое", async || {
            let st = job_stats(&self.db).await.unwrap();
            let ws = w.status();
            st.queued == 0 && st.running == 0 && !ws.batch_running && ws.rollups_pending == 0
        })
        .await;
    }

    /// [`Env::quiet`] по базе `db` (другого пользователя).
    async fn quiet_on(&self, w: &WorkerHandle, db: &Db) {
        self.until("воркер в покое", async || {
            let st = job_stats(db).await.unwrap();
            let ws = w.status();
            st.queued == 0 && st.running == 0 && !ws.batch_running && ws.rollups_pending == 0
        })
        .await;
    }

    /// Запрос reindex воркеру и его итог.
    async fn reindex(&self, w: &WorkerHandle, req: ReindexRequest) -> zam_index::ReindexReport {
        let prev = w.status().last_reindex.map(|o| o.finished_at);
        w.request_reindex(req);
        self.reindexed(w, prev).await
    }

    /// Ждёт итога reindex, завершённого позже `prev`.
    async fn reindexed(
        &self,
        w: &WorkerHandle,
        prev: Option<jiff::Timestamp>,
    ) -> zam_index::ReindexReport {
        self.until("reindex завершён", async || {
            let st = w.status();
            st.last_reindex.as_ref().map(|o| o.finished_at) != prev
                && !st.reindex_running
                && st.reindex_queued == 0
        })
        .await;
        w.status().last_reindex.unwrap().result.unwrap()
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

    // свёртка папки docs (дебаунс после задания a) тоже вызывает модель — для L0 папки.
    // Замер «до» — только когда воркер в покое: иначе под нагрузкой свёртка срабатывала
    // между замером и проверкой (а при быстрой переиндексации дебаунс отодвигал её за
    // проверку) — отсюда нестабильность теста.
    e.quiet(&w).await;
    assert_eq!(e.count(None, Some("docs"), &["folder"]).await, 1);
    // та же эмбеддируемая строка (переиндексация того же содержимого): модель не вызывается
    let before = e.fake.texts.load(Ordering::SeqCst);
    enqueue_reindex(&e.db, None).await.unwrap();
    w.notify();
    e.indexed(a).await;
    assert_eq!(e.fake.texts.load(Ordering::SeqCst), before);
    assert_eq!(w.embedded_texts(), before);
    // тот же текст в другой заметке — другой context_prefix, отдельный вызов модели
    let b = e.note(e.knowledge, "docs/b", NOTE).await;
    w.notify();
    e.indexed(b).await;
    assert!(
        w.embedded_texts() > before,
        "другой префикс — другой вектор"
    );

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
    delete_node(&e.db, a, zam_store::AuthorKind::System)
        .await
        .unwrap();
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

/// Одинаковая эмбеддируемая строка внутри пачки считается моделью один раз; ключ кэша —
/// `embed_key` (blake3 строки `context_prefix + "\n" + text`); пустая заметка — `indexed`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_embed_input_in_batch_is_embedded_once_and_empty_note_indexed() {
    let e = env("dedup").await;
    // две одинаковые секции под одним заголовком: одинаковые префикс и текст;
    // ручной L0 — без предложений-кандидатов
    let sec = "Повторяющийся абзац о сборке проекта и его тестах. ".repeat(12);
    let content = format!("---\nl0: Ручная\n---\n# Раздел\n\n{sec}\n\n# Раздел\n\n{sec}\n");
    let n = e.note(e.knowledge, "dup", &content).await;
    let empty = e.note(e.knowledge, "empty", "").await;
    let w = e.spawn();
    e.indexed(n).await;
    e.indexed(empty).await;
    let chunks = chunks_for_index(&e.db, n).await.unwrap();
    let keys: std::collections::HashSet<String> = chunks.iter().map(embed_key).collect();
    assert!(
        keys.len() < chunks.len(),
        "в заметке нет повторов: {}",
        chunks.len()
    );
    let ek = chunks_for_index(&e.db, empty).await.unwrap();
    let all: std::collections::HashSet<String> = keys
        .iter()
        .cloned()
        .chain(ek.iter().map(embed_key))
        .collect();
    assert_eq!(w.embedded_texts(), all.len() as u64);
    // ключ — blake3 ровно той строки, что ушла в модель
    let c = &chunks[0];
    assert_eq!(
        embed_key(c),
        blake3::hash(format!("{}\n{}", c.context_prefix, c.text).as_bytes())
            .to_hex()
            .to_string()
    );
    w.shutdown().await;
    assert!(!w.status().running && !w.status().vector_ready);
}

/// Сбой одной базы при пересборке: остальные базы пересобраны один раз и индексируются,
/// повтор — только для сбойной.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rebuild_skips_broken_user_db_and_does_not_repeat_others() {
    let e = env("rebuild-bad").await;
    let a = e.note(e.knowledge, "a", NOTE).await;
    // второй пользователь с повреждённым файлом базы
    let bad = e.system.create_user("bad", false).await.unwrap().id;
    std::fs::write(
        e.users_dir.join(format!("{bad}.db")),
        b"not a sqlite database at all",
    )
    .unwrap();
    let w = e.spawn();
    e.indexed(a).await;
    w.request_rebuild();
    // пересборка поставила задание для a: оно снова проиндексировано
    e.until("reindex a", async || {
        w.jobs_done() >= 2 && e.state(a).await == "indexed"
    })
    .await;
    let done = w.jobs_done();
    // новая запись индексируется, пока сбойная база повторяется
    let b = e
        .note(e.knowledge, "b", "Заметка во время повторов пересборки.\n")
        .await;
    w.notify();
    e.indexed(b).await;
    // несколько периодов повтора (retry_max = 500 мс): a повторно не перечанковывается
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(w.jobs_done(), done + 1);
    assert_eq!(e.state(a).await, "indexed");
    assert_eq!(job_stats(&e.db).await.unwrap().queued, 0);
    // файл не SQLite — повреждение навсегда: база исключена сразу, видна в статусе и аудите
    let skipped = w.status().rebuild_skipped;
    assert_eq!(skipped.len(), 1, "{skipped:?}");
    assert_eq!((skipped[0].user, skipped[0].attempts), (bad, 1));
    let audit = e.system.list_audit(50, None).await.unwrap();
    assert_eq!(
        audit
            .iter()
            .filter(|r| r.action == "index.rebuild_skipped" && r.user_id == Some(bad))
            .count(),
        1
    );
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
    assert_eq!(w.jobs_requeued(), 0);

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
    // задание пачки возвращено в очередь записью, получившей Unavailable
    assert!(w.jobs_requeued() >= 1, "{}", w.jobs_requeued());
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

/// Ключ кэша в `chunks.embed_key` — ровно значение помощника `embed_key` (единая
/// формула): после записи, правки и переноса (префикс меняется).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chunk_embed_key_column_equals_helper() {
    // без Qdrant: только база
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("zam-index-worker")
        .join(format!("ekey-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let dd = DataDir::open(&dir).unwrap();
    let db = UserDbs::new(dd.users_dir()).get(Id::new()).await.unwrap();
    let knowledge = list_spaces(&db)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.kind == SpaceKind::Knowledge)
        .unwrap()
        .id;
    let a = create_note(
        &db,
        NewNote {
            space_id: knowledge,
            path: NodePath::parse("x/a").unwrap(),
            content: NOTE.into(),
            l0: None,
            attrib: Attribution::new(AuthorKind::Agent),
        },
    )
    .await
    .unwrap()
    .id;
    let check = async |n: Id| {
        let raw: Vec<(String, Option<String>)> = db
            .read(move |c| {
                let mut st = c.prepare("SELECT id, embed_key FROM chunks WHERE node_id = ?1")?;
                let rows = st.query_map([n], |r| Ok((r.get(0)?, r.get(1)?)))?;
                Ok(rows.collect::<Result<_, _>>()?)
            })
            .await
            .unwrap();
        let chunks = chunks_for_index(&db, n).await.unwrap();
        assert_eq!(raw.len(), chunks.len());
        for c in &chunks {
            let col = raw.iter().find(|r| r.0 == c.id).unwrap().1.clone();
            assert_eq!(col.as_deref(), Some(embed_key(c).as_str()), "{}", c.id);
            assert_eq!(c.embed_key, embed_key(c));
        }
    };
    check(a).await;
    update_note(
        &db,
        a,
        1,
        "# Другое\n\nНовый текст.\n".into(),
        None,
        Attribution::new(AuthorKind::Agent),
    )
    .await
    .unwrap();
    check(a).await;
    move_node(
        &db,
        a,
        NodePath::parse("y/b").unwrap(),
        zam_store::AuthorKind::System,
    )
    .await
    .unwrap();
    check(a).await;
}

/// Reindex по запросу из кэша: модель не вызывается; область по пространству не трогает
/// другие; заметка без эмбеддинга в кэше получает задание (инференс только для неё);
/// сироты кэша убираются, точки папок восстанавливаются.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reindex_from_cache_without_inference() {
    let e = env("reidx").await;
    let a = e.note(e.knowledge, "docs/a", NOTE).await;
    let b = e
        .note(
            e.knowledge,
            "docs/b",
            "Вторая заметка про деплой и релизы.\n",
        )
        .await;
    let r = e
        .note(e.rules, "rule", "Правило: тесты перед коммитом.\n")
        .await;
    // сборка мусора в простое — редко: сироту должен убрать именно reindex
    let w = e.spawn_gc(e.vector.clone(), Duration::from_secs(3600));
    for n in [a, b, r] {
        e.indexed(n).await;
    }
    e.quiet(&w).await;
    assert_eq!(e.count(None, Some("docs"), &["folder"]).await, 1);
    let before = w.embedded_texts();
    let know = e.count(Some(e.knowledge), None, &[]).await;

    // пространство Rules: только его точки
    let rep = e
        .reindex(
            &w,
            ReindexRequest {
                user: Some(e.user),
                space: Some(e.rules),
                full: false,
            },
        )
        .await;
    assert_eq!((rep.users, rep.nodes, rep.misses), (1, 1, 0));
    assert_eq!(rep.points, e.chunks(r).await);
    assert!(!rep.recreated);
    assert_eq!(e.count(Some(e.rules), None, &[]).await, e.chunks(r).await);
    assert_eq!(e.count(Some(e.knowledge), None, &[]).await, know);
    assert_eq!(w.embedded_texts(), before);

    // кэш: у b пропал эмбеддинг одного чанка, плюс сирота без ссылок
    let bk = chunks_for_index(&e.db, b).await.unwrap()[0]
        .embed_key
        .clone();
    let model = "fake/test@00000001";
    let k2 = bk.clone();
    e.db.write(move |tx| {
        tx.execute("DELETE FROM embeddings WHERE text_hash = ?1", [k2])?;
        Ok(())
    })
    .await
    .unwrap();
    // L0 папки docs тоже пропал из кэша (как строка до v5, снятая сборкой мусора)
    let docs = get_node(&e.db, a).await.unwrap().parent_id.unwrap();
    e.db.write(move |tx| {
        tx.execute("DELETE FROM embeddings WHERE owner = ?1", [docs])?;
        Ok(())
    })
    .await
    .unwrap();
    put_embeddings(
        &e.db,
        model,
        vec![(
            "orphan".into(),
            Embedding {
                dense: vec![0.1; DIM],
                sparse: vec![],
            },
        )],
    )
    .await
    .unwrap();
    // все точки пользователя удалены (как после сбоя): reindex всей базы
    e.vector.delete_scope(&e.whole_base().await).await.unwrap();
    let rep = e
        .reindex(
            &w,
            ReindexRequest {
                user: Some(e.user),
                space: None,
                full: false,
            },
        )
        .await;
    assert_eq!((rep.nodes, rep.misses, rep.jobs, rep.folders), (2, 1, 1, 0));
    assert_eq!(rep.folder_misses, 1);
    assert_eq!(rep.missed_folders.len(), 1);
    // свёртка папки без кэша видна в статусе сразу с итогом reindex (или уже выполнена)
    let pending = w.status().rollups_pending;
    assert!(
        pending >= 1 || e.count(None, Some("docs"), &["folder"]).await == 1,
        "rollups_pending = {pending} до свёртки папки"
    );
    assert_eq!(rep.points, e.chunks(a).await + e.chunks(r).await);
    assert!(rep.gc_deleted >= 1, "{rep:?}");
    assert!(
        get_embeddings(&e.db, model, &["orphan".into()])
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(pending_reindex(&e.db).await.unwrap(), None);
    // b — воркером (инференс только для пропавшего чанка), точка папки — свёрткой
    // (инференс L0 папки)
    e.indexed(b).await;
    e.until("точки b и папки", async || {
        e.count(None, Some("docs/b"), &[]).await == e.chunks(b).await
            && e.count(None, Some("docs"), &["folder"]).await == 1
    })
    .await;
    e.quiet(&w).await;
    assert_eq!(w.embedded_texts(), before + 2);
    w.shutdown().await;
}

/// Коллекция другой модели: векторный канал выключен до reindex; reindex пересоздаёт её
/// для текущей модели и восстанавливает точки всех баз.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reindex_recreates_foreign_model_collection() {
    let e = env("reforeign").await;
    let a = e.note(e.knowledge, "a", NOTE).await;
    // эмбеддинги текущей модели уже есть (индексировали раньше): reindex без инференса
    {
        let w = e.spawn();
        e.indexed(a).await;
        w.shutdown().await;
    }
    e.vector.drop_all().await.unwrap();
    e.vector.ensure("other/model@ffff", DIM).await.unwrap();
    let w = e.spawn();
    e.until("mismatch", async || w.status().model_mismatch.is_some())
        .await;
    let before = w.embedded_texts();
    let rep = e
        .reindex(
            &w,
            ReindexRequest {
                user: Some(e.user),
                space: Some(e.knowledge),
                full: false,
            },
        )
        .await;
    assert!(rep.recreated);
    assert_eq!(rep.points, e.chunks(a).await);
    e.until("канал включён", async || {
        let st = w.status();
        st.vector_ready && st.model_mismatch.is_none()
    })
    .await;
    let meta = e.vector.ensure("fake/test@00000001", DIM).await.unwrap();
    assert_eq!(meta.model_id.as_deref(), Some("fake/test@00000001"));
    assert_eq!(e.count(None, Some("a"), &[]).await, e.chunks(a).await);
    assert_eq!(w.embedded_texts(), before);
    w.shutdown().await;
}

/// Прерванный reindex (отметка `reindex_pending` осталась) повторяется при запуске воркера.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupted_reindex_is_resumed_on_start() {
    let e = env("resume").await;
    let a = e.note(e.knowledge, "a", NOTE).await;
    {
        let w = e.spawn();
        e.indexed(a).await;
        w.shutdown().await;
    }
    // точки удалены, отметка стоит — как при остановке посреди reindex
    e.vector.delete_scope(&e.whole_base().await).await.unwrap();
    mark_reindex(&e.db, Some("{\"full\":false,\"space\":null}".into()))
        .await
        .unwrap();
    let w = e.spawn();
    let rep = e.reindexed(&w, None).await;
    assert_eq!(rep.nodes, 1);
    assert_eq!(e.count(None, Some("a"), &[]).await, e.chunks(a).await);
    assert_eq!(pending_reindex(&e.db).await.unwrap(), None);
    assert_eq!(w.embedded_texts(), 0);
    w.shutdown().await;

    // прерван при чужой модели коллекции: ворота заданий закрыты, но отметка читается
    // при запуске, reindex пересоздаёт коллекцию
    e.vector.drop_all().await.unwrap();
    e.vector.ensure("other/model@ffff", DIM).await.unwrap();
    mark_reindex(&e.db, Some("{\"full\":false,\"space\":null}".into()))
        .await
        .unwrap();
    let w = e.spawn();
    let rep = e.reindexed(&w, None).await;
    assert!(rep.recreated);
    assert_eq!(e.count(None, Some("a"), &[]).await, e.chunks(a).await);
    e.until("канал включён", async || {
        let st = w.status();
        st.vector_ready && st.model_mismatch.is_none()
    })
    .await;
    assert_eq!(pending_reindex(&e.db).await.unwrap(), None);
    assert_eq!(w.embedded_texts(), 0);
    w.shutdown().await;
}

/// Сборка мусора кэша в простое: сироты уходят, живые ключи остаются.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_gc_removes_orphan_embeddings() {
    let e = env("gc").await;
    let a = e.note(e.knowledge, "a", NOTE).await;
    let w = e.spawn();
    e.indexed(a).await;
    let live = e.cached().await;
    put_embeddings(
        &e.db,
        "fake/test@00000001",
        (0..5)
            .map(|i| {
                (
                    format!("orphan{i}"),
                    Embedding {
                        dense: vec![0.1; DIM],
                        sparse: vec![],
                    },
                )
            })
            .collect(),
    )
    .await
    .unwrap();
    // правка: ключи прежней ревизии тоже становятся сиротами
    update_note(
        &e.db,
        a,
        1,
        "Совсем новый текст заметки.\n".into(),
        None,
        Attribution::new(AuthorKind::Agent),
    )
    .await
    .unwrap();
    w.notify();
    e.indexed(a).await;
    let old = live;
    e.until("сироты убраны", async || {
        get_embeddings(&e.db, "fake/test@00000001", &["orphan0".into()])
            .await
            .unwrap()
            .is_empty()
    })
    .await;
    // живые ключи текущей ревизии на месте; строк не больше, чем было до сирот
    let mut keys: Vec<String> = chunks_for_index(&e.db, a)
        .await
        .unwrap()
        .iter()
        .map(|c| c.embed_key.clone())
        .collect();
    keys.sort();
    keys.dedup();
    let have = get_embeddings(&e.db, "fake/test@00000001", &keys)
        .await
        .unwrap();
    assert_eq!(have.len(), keys.len());
    e.until(
        "ключи прежней ревизии убраны",
        async || e.cached().await <= old,
    )
    .await;
    w.shutdown().await;
}

/// Замер для отчёта: reindex из кэша `embeddings` 20 000 синтетических чанков (dense 1024,
/// как bge-m3, и sparse на 40 токенов) в настоящий Qdrant, без инференса. Цель — не меньше
/// 2000 чанков/с. Запуск:
/// `cargo test --release -p zam-index --test worker -- --ignored bench_reindex --nocapture`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn bench_reindex_from_embeddings() {
    const BENCH_DIM: usize = 1024;
    const MODEL_B: &str = "bench/bge@00000000";
    const TARGET: u64 = 20_000;
    let e = env("bench").await;
    // заметки по 16 разделов до 20 000 чанков (листы, L1 разделов и заметки)
    let section = "Раздел о сборке и тестах проекта: команды, переменные окружения, каталоги \
                   журналов и временных файлов, порядок запуска и проверки результатов. "
        .repeat(6);
    let mut total = 0u64;
    let mut notes = 0u32;
    let setup = Instant::now();
    while total < TARGET {
        let body: String = (0..16)
            .map(|i| format!("## Раздел {i} заметки {notes}\n\n{section}\n\n"))
            .collect();
        let n = e
            .note(
                e.knowledge,
                &format!("bench/d{}/n{notes}", notes % 20),
                &body,
            )
            .await;
        total += chunks_for_index(&e.db, n).await.unwrap().len() as u64;
        notes += 1;
    }
    // кэш: эмбеддинг на каждый ключ чанка
    let mut keys: Vec<String> = Vec::new();
    let mut after = None;
    loop {
        let page = zam_store::reindex_page(&e.db, None, after, 500)
            .await
            .unwrap();
        let Some(last) = page.last() else { break };
        after = Some(last.node_id);
        keys.extend(
            page.iter()
                .flat_map(|n| n.chunks.iter().map(|c| c.embed_key.clone())),
        );
    }
    keys.sort();
    keys.dedup();
    for part in keys.chunks(1000) {
        let rows = part
            .iter()
            .enumerate()
            .map(|(i, k)| {
                let s = i as f32 * 0.013;
                (
                    k.clone(),
                    Embedding {
                        dense: (0..BENCH_DIM)
                            .map(|j| ((j as f32) * 0.37 + s).sin() * 0.03)
                            .collect(),
                        sparse: (0..40u32).map(|t| (t * 997 + i as u32, 0.1)).collect(),
                    },
                )
            })
            .collect();
        put_embeddings(&e.db, MODEL_B, rows).await.unwrap();
    }
    e.vector.ensure(MODEL_B, BENCH_DIM).await.unwrap();
    eprintln!(
        "T10-BENCH подготовка: заметок {notes}, чанков {total}, ключей {}, {} с",
        keys.len(),
        setup.elapsed().as_secs()
    );
    let ctx = zam_index::ReindexCtx {
        system: &e.system,
        users: &e.users,
        vector: e.vector.as_ref(),
        model_id: MODEL_B,
        dim: BENCH_DIM,
        embedder: None,
    };
    let req = ReindexRequest {
        user: Some(e.user),
        space: None,
        full: false,
    };
    for run in 1..=2 {
        let t = Instant::now();
        let rep = zam_index::reindex(&ctx, &req).await.unwrap();
        let secs = t.elapsed().as_secs_f64();
        assert_eq!(rep.points, total);
        assert_eq!(rep.misses, 0);
        eprintln!(
            "T10-BENCH прогон {run}: точек {} за {:.2} с — {:.0} чанков/с (чтение {} мс, \
             удаление и запись {} мс)",
            rep.points,
            secs,
            rep.points as f64 / secs,
            rep.read_ms,
            rep.write_ms
        );
    }
    assert_eq!(
        e.count(None, Some("bench"), &["chunk", "section", "note"])
            .await,
        total
    );
}

/// Исключённая из пересборки база (п. 2 ревью): отметка в `system.db` переживает
/// перезапуск; после восстановления файла базы воркер сам ставит ей reindex из кэша —
/// точки возвращаются без инференса, отметка снимается.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skipped_db_survives_restart_and_is_reindexed_after_restore() {
    let e = env("skiprst").await;
    let b = e.system.create_user("b", false).await.unwrap().id;
    let file = e.users_dir.join(format!("{b}.db"));
    let keep = e.users_dir.join(format!("{b}.keep"));
    // пространства b — до порчи файла (отдельный кэш баз закрывает файл при выходе)
    let b_all = {
        let users = UserDbs::new(e.users_dir.clone());
        VectorFilter::scope(b, all_spaces(&users.get(b).await.unwrap()).await)
    };
    let b_points = async || e.vector.count(&b_all).await.unwrap();
    // b индексируется воркером на отдельном кэше баз: файл закроется вместе с ним
    let pts = {
        let users = UserDbs::new(e.users_dir.clone());
        let db = users.get(b).await.unwrap();
        let k = list_spaces(&db)
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.kind == SpaceKind::Knowledge)
            .unwrap()
            .id;
        let n = create_note(
            &db,
            NewNote {
                space_id: k,
                path: NodePath::parse("bn").unwrap(),
                content: NOTE.into(),
                l0: None,
                attrib: Attribution::new(AuthorKind::Agent),
            },
        )
        .await
        .unwrap()
        .id;
        let w = e.spawn_on(users.clone());
        e.until("b indexed", async || {
            get_node(&db, n).await.unwrap().index_state == "indexed"
        })
        .await;
        e.quiet_on(&w, &db).await;
        let pts = b_points().await;
        assert!(pts > 0);
        w.shutdown().await;
        pts
    };
    // копия базы b; хранилище «стёрто» (точек b нет), файл b повреждён
    std::fs::copy(&file, &keep).unwrap();
    e.vector.delete_scope(&b_all).await.unwrap();
    std::fs::write(&file, b"not a sqlite database at all").unwrap();
    {
        let w = e.spawn_on(UserDbs::new(e.users_dir.clone()));
        w.request_rebuild();
        e.until("b исключена", async || {
            w.status().rebuild_skipped.iter().any(|d| d.user == b)
        })
        .await;
        w.shutdown().await;
    }
    let skipped = e.system.index_skipped().await.unwrap();
    assert_eq!(
        skipped.iter().map(|s| s.user_id).collect::<Vec<_>>(),
        vec![b]
    );
    // перезапуск с той же повреждённой базой: исключение видно сразу (из system.db)
    {
        let w = e.spawn_on(UserDbs::new(e.users_dir.clone()));
        e.until(
            "исключение после перезапуска",
            async || w.status().rebuild_skipped.iter().any(|d| d.user == b),
        )
        .await;
        w.shutdown().await;
    }
    assert_eq!(b_points().await, 0);
    // база восстановлена из копии, перезапуск: reindex из кэша без инференса
    std::fs::copy(&keep, &file).unwrap();
    let w = e.spawn_on(UserDbs::new(e.users_dir.clone()));
    e.until("точки b восстановлены", async || {
        b_points().await == pts
    })
    .await;
    e.until("отметка снята", async || {
        w.status().rebuild_skipped.is_empty()
    })
    .await;
    assert!(e.system.index_skipped().await.unwrap().is_empty());
    assert_eq!(w.embedded_texts(), 0);
    w.shutdown().await;
}
