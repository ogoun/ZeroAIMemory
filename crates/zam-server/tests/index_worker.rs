//! Модель, Qdrant и воркер индексации в сервере (задача 7 M2): MiniLM на CPU-сборке ORT
//! и настоящий Qdrant 1.19.2 (внешний процесс теста или управляемый супервизором).
//!
//! Без ORT или MiniLM тесты пропускаются; в CI (`ZAM_REQUIRE_ML=1`) — падают.

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use axum::http::{Method, StatusCode};
use common::api::{Api, api};
use common::qdrant::{TestQdrant, free_port, qdrant_bin};
use serde_json::{Value, json};
use zam_core::Id;
use zam_index::VectorFilter;
use zam_server::config::{MlDevice, QdrantMode, ServerConfig};
use zam_server::index_state::DegradedReason;
use zam_store::{SpaceKind, chunks_for_index, get_node, job_stats, list_spaces};

const MINILM: &str = "minilm-l6-int8";
/// Предел ожидания готовности коллекции (создание payload-индексов на HDD — секунды).
const READY: Duration = Duration::from_secs(90);

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn platform() -> &'static str {
    if cfg!(windows) {
        "windows-x64"
    } else {
        "linux-x64"
    }
}

fn ort_cpu() -> PathBuf {
    let name = if cfg!(windows) {
        "onnxruntime.dll"
    } else {
        "libonnxruntime.so"
    };
    repo()
        .join("target")
        .join("deps")
        .join(platform())
        .join("onnxruntime")
        .join(name)
}

fn models() -> PathBuf {
    repo().join("target").join("models")
}

/// ORT, модель и Qdrant на месте; иначе пропуск (в CI — падение).
fn have(model: &str) -> bool {
    let ok = ort_cpu().is_file()
        && models().join(model).join("model.toml").is_file()
        && qdrant_bin().is_file();
    if !ok {
        assert!(
            std::env::var_os("ZAM_REQUIRE_ML").is_none(),
            "нет ORT, модели {model} или Qdrant: cargo xtask fetch-deps && cargo xtask fetch-models --test"
        );
        eprintln!("пропуск: нет ORT, модели {model} или Qdrant");
    }
    ok
}

/// ML из `target/`: CPU-сборка ORT (одна на процесс тестов), модель `model`.
fn ml(c: &mut ServerConfig, model: &str) {
    c.ml.device = MlDevice::Cpu;
    c.ml.ort_dylib = Some(ort_cpu());
    c.ml.models_dir = Some(models());
    c.ml.embed_model = Some(model.into());
    c.ml.index_threads = 2;
    c.index.rollup_debounce_ms = 200;
}

/// Внешний Qdrant теста.
fn external(c: &mut ServerConfig, q: &TestQdrant) {
    c.qdrant.mode = QdrantMode::External;
    c.qdrant.url = Some(q.grpc_url());
    c.qdrant.api_key = Some(q.api_key.clone());
}

/// Ждёт включения векторного канала (коллекция создана и проверена).
async fn vector_ready(a: &Api) {
    let t = Instant::now();
    while a.st.index.vector_available().is_err() {
        assert!(
            t.elapsed() < READY,
            "векторный канал не включился: {:?}",
            a.st.index.vector_available()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn uid(a: &Api) -> Id {
    a.st.admin_id
}

async fn space(a: &Api, kind: SpaceKind) -> Id {
    let db = a.st.users.get(uid(a)).await.unwrap();
    list_spaces(&db)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.kind == kind)
        .unwrap()
        .id
}

async fn state(a: &Api, node: &str) -> String {
    let db = a.st.users.get(uid(a)).await.unwrap();
    get_node(&db, node.parse().unwrap())
        .await
        .unwrap()
        .index_state
}

/// Число точек заметок (без точек папок) по пространству и префиксу пути.
async fn points(a: &Api, space: Option<Id>, prefix: Option<&str>) -> u64 {
    let f = VectorFilter {
        user_id: uid(a),
        space_ids: space.into_iter().collect(),
        path_prefix: prefix.map(str::to_string),
        levels: ["chunk", "section", "note"].map(String::from).to_vec(),
    };
    a.st.index.vector.as_ref().unwrap().count(&f).await.unwrap()
}

async fn chunk_count(a: &Api, node: &str) -> u64 {
    let db = a.st.users.get(uid(a)).await.unwrap();
    chunks_for_index(&db, node.parse().unwrap())
        .await
        .unwrap()
        .len() as u64
}

async fn until(what: &str, limit: Duration, f: impl AsyncFn() -> bool) {
    let t = Instant::now();
    while !f().await {
        assert!(t.elapsed() < limit, "не дождались за {limit:?}: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn id(v: &Value) -> String {
    v["id"].as_str().unwrap().to_string()
}

fn body(i: usize) -> String {
    format!(
        "# Заметка {i}\n\nПроект номер {i} собирается командой cargo build, тесты идут через cargo test. \
         Конфигурация лежит в каталоге deploy, журнал пишется в logs.\n\n## Детали {i}\n\n\
         Переменные окружения задаёт скрипт dev-env; временные файлы — только внутри .tmp.\n"
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn twenty_notes_indexed_then_edit_and_delete_replace_points() {
    if !have(MINILM) {
        return;
    }
    let q = TestQdrant::start("srv-twenty");
    let a = api("idx-twenty", |c| {
        ml(c, MINILM);
        external(c, &q);
    })
    .await;
    assert_eq!(
        a.st.index.model_id().map(|m| m.starts_with(MINILM)),
        Some(true)
    );
    vector_ready(&a).await;

    let t0 = Instant::now();
    let mut ids = Vec::new();
    for i in 0..20 {
        ids.push(id(&a
            .note("knowledge", &format!("bulk/n{i:02}"), &body(i))
            .await));
    }
    let written = t0.elapsed();
    until(
        "20 заметок indexed",
        Duration::from_secs(10),
        async || {
            for n in &ids {
                if state(&a, n).await != "indexed" {
                    return false;
                }
            }
            true
        },
    )
    .await;
    let indexed = t0.elapsed();
    eprintln!(
        "T7-METRIC minilm: запись 20 заметок {} мс, все indexed через {} мс, текстов в модели {}",
        written.as_millis(),
        indexed.as_millis(),
        a.st.index.worker.embedded_texts()
    );
    let mut total = 0;
    for n in &ids {
        total += chunk_count(&a, n).await;
    }
    assert_eq!(points(&a, None, Some("bulk")).await, total);
    // папка bulk: L0-свёртка детей и точка уровня folder
    until(
        "свёртка папки",
        Duration::from_secs(10),
        async || {
            let f = VectorFilter {
                user_id: uid(&a),
                space_ids: vec![],
                path_prefix: Some("bulk".into()),
                levels: vec!["folder".into()],
            };
            a.st.index.vector.as_ref().unwrap().count(&f).await.unwrap() == 1
        },
    )
    .await;

    // правка заменяет точки заметки: старых chunk_id в выдаче нет
    let n0 = &ids[0];
    let old: Vec<String> = {
        let db = a.st.users.get(uid(&a)).await.unwrap();
        chunks_for_index(&db, n0.parse().unwrap())
            .await
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect()
    };
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &format!("/api/v1/nodes/{n0}"),
            Some(json!({"content": "Совершенно новый текст о выпуске релиза и тегах git.\n"})),
            &[("if-match", "1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    until(
        "правка проиндексирована",
        Duration::from_secs(10),
        async || {
            state(&a, n0).await == "indexed"
                && points(&a, None, Some("bulk/n00")).await == chunk_count(&a, n0).await
        },
    )
    .await;
    let emb = a.st.index.embedder.clone().unwrap();
    let qv = emb.embed_query("выпуск релиза и теги git").unwrap();
    let f = VectorFilter {
        user_id: uid(&a),
        space_ids: vec![],
        path_prefix: Some("bulk/n00".into()),
        levels: vec![],
    };
    let hits =
        a.st.index
            .vector
            .as_ref()
            .unwrap()
            .search(&qv.dense, &qv.sparse, &f)
            .await
            .unwrap();
    assert!(!hits.is_empty());
    assert!(hits.iter().all(|h| !old.contains(&h.chunk_id)), "{hits:?}");

    // удаление удаляет точки
    let r = a
        .t(Method::DELETE, &format!("/api/v1/nodes/{}", ids[1]), None)
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    until(
        "точки удалены",
        Duration::from_secs(10),
        async || points(&a, None, Some("bulk/n01")).await == 0,
    )
    .await;
    a.st.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn same_text_in_two_notes_is_embedded_once() {
    if !have(MINILM) {
        return;
    }
    let q = TestQdrant::start("srv-dedup");
    let a = api("idx-dedup", |c| {
        ml(c, MINILM);
        external(c, &q);
    })
    .await;
    vector_ready(&a).await;
    let x = id(&a.note("knowledge", "x", &body(7)).await);
    until("x indexed", Duration::from_secs(10), async || {
        state(&a, &x).await == "indexed"
    })
    .await;
    let before = a.st.index.worker.embedded_texts();
    assert!(before > 0);
    let y = id(&a.note("knowledge", "y", &body(7)).await);
    until("y indexed", Duration::from_secs(10), async || {
        state(&a, &y).await == "indexed"
    })
    .await;
    assert_eq!(a.st.index.worker.embedded_texts(), before);
    assert_eq!(points(&a, None, Some("y")).await, chunk_count(&a, &y).await);
    a.st.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn human_and_index_false_never_reach_qdrant() {
    if !have(MINILM) {
        return;
    }
    let q = TestQdrant::start("srv-excl");
    let a = api("idx-excl", |c| {
        ml(c, MINILM);
        external(c, &q);
    })
    .await;
    vector_ready(&a).await;
    let r = a
        .adm(
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": "human", "path": "diary", "kind": "note", "content": body(1)})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let off = id(&a
        .note(
            "knowledge",
            "off",
            &format!("---\nindex: false\n---\n{}", body(2)),
        )
        .await);
    let on = id(&a.note("knowledge", "on", &body(3)).await);
    until("on indexed", Duration::from_secs(10), async || {
        state(&a, &on).await == "indexed"
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let human = space(&a, SpaceKind::Human).await;
    assert_eq!(points(&a, Some(human), None).await, 0);
    assert_eq!(points(&a, None, Some("off")).await, 0);
    assert_eq!(state(&a, &off).await, "excluded");
    assert!(points(&a, None, Some("on")).await > 0);
    // index: false после индексации — точки уходят
    let r = a
        .tok_h(
            &a.token,
            Method::PUT,
            &format!("/api/v1/nodes/{on}"),
            Some(json!({"content": format!("---\nindex: false\n---\n{}", body(3))})),
            &[("if-match", "1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    until(
        "точки index:false удалены",
        Duration::from_secs(10),
        async || points(&a, None, Some("on")).await == 0,
    )
    .await;
    a.st.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn qdrant_down_queues_jobs_fts_works_and_queue_catches_up() {
    if !have(MINILM) {
        return;
    }
    let mut q = TestQdrant::start("srv-down");
    let a = api("idx-down", |c| {
        ml(c, MINILM);
        external(c, &q);
    })
    .await;
    vector_ready(&a).await;
    q.stop();
    // запись и FTS работают без Qdrant
    let n = id(&a
        .note(
            "knowledge",
            "down/n",
            "# Простой\n\nЗаметка записана, пока Qdrant лежит: уникальноеслово.\n",
        )
        .await);
    let r = a
        .t(
            Method::POST,
            "/api/v1/search",
            Some(json!({"query": "уникальноеслово"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["items"][0]["path"], "down/n", "{}", r.text());
    until(
        "деградация замечена",
        Duration::from_secs(10),
        async || a.st.index.vector_available() == Err(DegradedReason::QdrantDown),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let db = a.st.users.get(uid(&a)).await.unwrap();
    let st = job_stats(&db).await.unwrap();
    assert_eq!((st.queued + st.running, st.failed), (1, 0), "{st:?}");
    assert_eq!(state(&a, &n).await, "pending");

    q.restart();
    until(
        "очередь догнала",
        Duration::from_secs(30),
        async || state(&a, &n).await == "indexed",
    )
    .await;
    assert!(a.st.index.vector_available().is_ok());
    assert!(points(&a, None, Some("down")).await > 0);
    a.st.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn server_without_models_starts_in_fts_only() {
    if !have(MINILM) {
        return;
    }
    let empty = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("idx-no-models");
    let _ = std::fs::remove_dir_all(&empty);
    std::fs::create_dir_all(&empty).unwrap();
    let a = api("idx-nomodel", |c| {
        ml(c, MINILM);
        c.ml.models_dir = Some(empty.clone());
        c.ml.embed_model = None;
    })
    .await;
    assert_eq!(a.st.index.vector_available(), Err(DegradedReason::NoModel));
    assert!(a.st.index.embedder.is_none());
    assert!(
        a.st.index
            .ml_error
            .as_deref()
            .is_some_and(|e| e.contains("bge-m3-int8"))
    );
    // Qdrant без модели не запускается
    assert!(a.st.index.supervisor().is_none());
    let n = a
        .note(
            "knowledge",
            "fts",
            "# FTS\n\nТолько полнотекстовый поиск.\n",
        )
        .await;
    let r = a
        .t(
            Method::POST,
            "/api/v1/search",
            Some(json!({"query": "полнотекстовый"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["items"][0]["path"], "fts");
    assert_eq!(state(&a, &id(&n)).await, "pending");
    a.st.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn foreign_model_collection_disables_vector_and_worker_does_not_write() {
    if !have(MINILM) {
        return;
    }
    let q = TestQdrant::start("srv-mismatch");
    {
        use zam_index::{QdrantIndex, VectorIndex};
        let c = QdrantIndex::connect(&q.grpc_url(), Some(&q.api_key)).unwrap();
        c.ensure("bge-m3-int8@deadbeef", 384).await.unwrap();
    }
    let a = api("idx-mismatch", |c| {
        ml(c, MINILM);
        external(c, &q);
    })
    .await;
    until("mismatch", READY, async || {
        a.st.index.vector_available() == Err(DegradedReason::ModelMismatch)
    })
    .await;
    let (coll, cur) = a.st.index.model_mismatch().unwrap();
    assert_eq!(coll, "bge-m3-int8@deadbeef");
    assert!(cur.starts_with(MINILM));
    let n = id(&a.note("knowledge", "m", &body(5)).await);
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(points(&a, None, None).await, 0);
    assert_eq!(state(&a, &n).await, "pending");
    assert_eq!(a.st.index.worker.embedded_texts(), 0);
    a.st.shutdown().await;
}

/// Хранилище управляемого Qdrant стёрто (метка старой версии): сервер ставит reindex
/// из `embeddings` (модель не вызывается), пишет аудит `index.storage_rebuilt` и
/// снимает флаг пересборки.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn managed_storage_rebuild_reindexes_from_embeddings_without_inference() {
    if !have(MINILM) {
        return;
    }
    let (http, grpc) = (free_port(), free_port());
    let dir = common::tmp("api-idx-rebuild");
    let managed = |c: &mut ServerConfig| {
        ml(c, MINILM);
        c.qdrant.mode = QdrantMode::Managed;
        c.qdrant.binary = Some(qdrant_bin());
        c.qdrant.http_port = http;
        c.qdrant.grpc_port = grpc;
    };
    let mut c = common::cfg_in(dir.clone());
    managed(&mut c);
    let a = common::api::api_with(c.clone()).await;
    vector_ready(&a).await;
    let n = id(&a.note("knowledge", "keep/n", &body(9)).await);
    until("n indexed", Duration::from_secs(10), async || {
        state(&a, &n).await == "indexed"
    })
    .await;
    let pts = points(&a, None, Some("keep")).await;
    assert!(pts > 0);
    a.st.shutdown().await;
    drop(a);

    // «откат» версии хранилища: при старте супервизор стирает storage/
    std::fs::write(
        dir.join("qdrant").join("zam-storage.json"),
        r#"{"qdrant_version":"1.16.0"}"#,
    )
    .unwrap();
    let b = common::api::api_with(c).await;
    until("точки восстановлены", READY, async || {
        b.st.index.vector_available().is_ok() && points(&b, None, Some("keep")).await == pts
    })
    .await;
    until("n indexed", Duration::from_secs(10), async || {
        state(&b, &n).await == "indexed"
    })
    .await;
    assert_eq!(
        b.st.index.worker.embedded_texts(),
        0,
        "пересборка без инференса"
    );
    let sup = b.st.index.supervisor().unwrap();
    assert!(!sup.storage_rebuilt());
    let audit = b.st.system.list_audit(50, None).await.unwrap();
    let ev = audit
        .iter()
        .find(|r| r.action == "index.storage_rebuilt")
        .expect("аудит index.storage_rebuilt");
    assert_eq!(ev.actor, "system");
    b.st.shutdown().await;
}

/// Замер для отчёта: 20 заметок на bge-m3 int8 (CPU). Запуск:
/// `cargo test -p zam-server --test index_worker -- --ignored bench_twenty --nocapture`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn bench_twenty_notes_bge_m3_int8() {
    if !have("bge-m3-int8") {
        return;
    }
    let q = TestQdrant::start("srv-bench-bge");
    let a = api("idx-bench-bge", |c| {
        ml(c, "bge-m3-int8");
        c.ml.index_threads = 0;
        external(c, &q);
    })
    .await;
    vector_ready(&a).await;
    let t0 = Instant::now();
    let mut ids = Vec::new();
    for i in 0..20 {
        ids.push(id(&a
            .note("knowledge", &format!("bulk/n{i:02}"), &body(i))
            .await));
    }
    until(
        "20 заметок indexed",
        Duration::from_secs(600),
        async || {
            for n in &ids {
                if state(&a, n).await != "indexed" {
                    return false;
                }
            }
            true
        },
    )
    .await;
    eprintln!(
        "T7-METRIC bge-m3-int8: 20 заметок indexed через {} мс, текстов в модели {}, устройство {:?}",
        t0.elapsed().as_millis(),
        a.st.index.worker.embedded_texts(),
        a.st.index.device
    );
    a.st.shutdown().await;
}
