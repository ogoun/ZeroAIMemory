//! Модель, Qdrant и воркер индексации в сервере (задача 7 M2): MiniLM на CPU-сборке ORT
//! и настоящий Qdrant 1.19.2 (внешний процесс теста или управляемый супервизором).
//!
//! Без ORT или MiniLM тесты пропускаются; в CI (`ZAM_REQUIRE_ML=1`) — падают
//! (помощники — `common::ml`).

mod common;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use axum::http::{Method, StatusCode};
use common::api::{Api, api};
use common::ml::{MINILM, READY, external, have, ml, until, vector_ready};
use common::qdrant::{TestQdrant, free_port, qdrant_bin};
use serde_json::{Value, json};
use zam_core::Id;
use zam_index::VectorFilter;
use zam_server::config::{QdrantMode, ServerConfig};
use zam_server::index_state::DegradedReason;
use zam_store::{SpaceKind, chunks_for_index, get_node, job_stats, list_spaces};

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
    let (embed, qdrant) = a.st.index.worker.busy();
    eprintln!(
        "T7-METRIC minilm: запись 20 заметок {} мс, все indexed через {} мс, текстов в модели {}, инференс {} мс, Qdrant {} мс",
        written.as_millis(),
        indexed.as_millis(),
        a.st.index.worker.embedded_texts(),
        embed.as_millis(),
        qdrant.as_millis()
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

/// Одинаковая эмбеддируемая строка считается моделью один раз (переиндексация того же
/// содержимого — без модели); тот же текст в другой заметке — другой префикс, отдельный вызов.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn same_embed_input_once_other_prefix_separately() {
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
    // переиндексация того же содержимого (reindex из кэша)
    let db = a.st.users.get(uid(&a)).await.unwrap();
    zam_store::enqueue_reindex(&db, None).await.unwrap();
    a.st.index.notify();
    until("x reindexed", Duration::from_secs(10), async || {
        state(&a, &x).await == "indexed" && job_stats(&db).await.unwrap().queued == 0
    })
    .await;
    assert_eq!(a.st.index.worker.embedded_texts(), before);
    // тот же текст в другой заметке
    let y = id(&a.note("knowledge", "y", &body(7)).await);
    until("y indexed", Duration::from_secs(10), async || {
        state(&a, &y).await == "indexed"
    })
    .await;
    assert!(a.st.index.worker.embedded_texts() > before);
    assert_eq!(points(&a, None, Some("y")).await, chunk_count(&a, &y).await);
    a.st.shutdown().await;
}

/// Перенос заметки из папки: свёртка L0 и точка старой папки пересчитываются.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn move_out_of_folder_updates_old_folder_rollup() {
    if !have(MINILM) {
        return;
    }
    let q = TestQdrant::start("srv-move");
    let a = api("idx-move", |c| {
        ml(c, MINILM);
        external(c, &q);
    })
    .await;
    vector_ready(&a).await;
    let n1 = id(&a.note("knowledge", "a/n1", &body(1)).await);
    let n2 = id(&a.note("knowledge", "a/n2", &body(2)).await);
    a.note("knowledge", "b/keep", &body(3)).await;
    let db = a.st.users.get(uid(&a)).await.unwrap();
    let folder_a = get_node(&db, n1.parse().unwrap())
        .await
        .unwrap()
        .parent_id
        .unwrap();
    let l0_a = async || {
        get_node(&db, folder_a)
            .await
            .unwrap()
            .l0
            .unwrap_or_default()
    };
    let folder_points = async |prefix: &str| {
        let f = VectorFilter {
            user_id: uid(&a),
            space_ids: vec![],
            path_prefix: Some(prefix.into()),
            levels: vec!["folder".into()],
        };
        a.st.index.vector.as_ref().unwrap().count(&f).await.unwrap()
    };
    until("свёртка a", Duration::from_secs(10), async || {
        let l = l0_a().await;
        l.contains("n1") && l.contains("n2") && folder_points("a").await == 1
    })
    .await;

    let mv = async |node: &str, to: &str| {
        let r = a
            .t(
                Method::POST,
                &format!("/api/v1/nodes/{node}/move"),
                Some(json!({ "path": to })),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    };
    mv(&n1, "b/n1").await;
    until(
        "строка n1 ушла из L0 папки a",
        Duration::from_secs(10),
        async || {
            let l = l0_a().await;
            !l.contains("n1") && l.contains("n2")
        },
    )
    .await;
    // последняя заметка ушла: L0 папки пуст, её точка удалена
    mv(&n2, "b/n2").await;
    until(
        "папка a пуста",
        Duration::from_secs(10),
        async || {
            get_node(&db, folder_a).await.unwrap().l0.is_none() && folder_points("a").await == 0
        },
    )
    .await;
    a.st.shutdown().await;
}

/// Каталог Qdrant занят другим супервизором: отдельная причина деградации.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn locked_qdrant_dir_is_reported_separately() {
    if !have(MINILM) {
        return;
    }
    let dir = common::tmp("api-idx-locked");
    let (http, grpc) = (free_port(), free_port());
    // чужой супервизор держит лок <data_dir>/qdrant (бинарника нет — процесса тоже)
    let mut m = zam_index::supervisor::ManagedConfig::new(dir.join("нет").join("qdrant"));
    m.http_port = http;
    m.grpc_port = grpc;
    let other = zam_index::supervisor::Supervisor::start(
        zam_index::supervisor::SupervisorConfig::Managed(m),
        &dir,
    )
    .await
    .unwrap();
    let mut c = common::cfg_in(dir);
    ml(&mut c, MINILM);
    c.qdrant.binary = Some(qdrant_bin());
    c.qdrant.http_port = http;
    c.qdrant.grpc_port = grpc;
    let a = common::api::api_with(c).await;
    assert_eq!(
        a.st.index.vector_available(),
        Err(DegradedReason::QdrantLocked)
    );
    assert_eq!(DegradedReason::QdrantLocked.as_str(), "qdrant_locked");
    a.st.shutdown().await;
    other.shutdown().await;
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
    // второй пользователь, чья база будет повреждена навсегда
    let bad = a.st.system.create_user("bad", false).await.unwrap().id;
    a.st.shutdown().await;
    drop(a);
    std::fs::write(
        dir.join("users").join(format!("{bad}.db")),
        b"not a sqlite database at all",
    )
    .unwrap();

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
    // повреждённая база исключена из пересборки, флаг пересборки снят — повторной
    // полной перечанковки на следующем старте не будет
    let sup = b.st.index.supervisor().unwrap();
    until(
        "флаг пересборки снят",
        READY,
        async || !sup.storage_rebuilt(),
    )
    .await;
    assert!(!dir.join("qdrant").join(".zam-reindex").exists());
    let audit = b.st.system.list_audit(50, None).await.unwrap();
    let ev = audit
        .iter()
        .find(|r| r.action == "index.storage_rebuilt")
        .expect("аудит index.storage_rebuilt");
    assert_eq!(ev.actor, "system");
    assert!(
        audit
            .iter()
            .any(|r| r.action == "index.rebuild_skipped" && r.user_id == Some(bad))
    );
    let skipped = b.st.index.worker.status().rebuild_skipped;
    assert_eq!(
        skipped.iter().map(|d| d.user).collect::<Vec<_>>(),
        vec![bad]
    );
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
