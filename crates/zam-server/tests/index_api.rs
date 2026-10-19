//! `/index/status`, `POST /admin/reindex` и `zam-server reindex` (задача 10 M2): MiniLM на
//! CPU-сборке ORT и настоящий Qdrant 1.19.2 (внешний процесс теста или управляемый).
//!
//! Без ORT или MiniLM тесты пропускаются; в CI (`ZAM_REQUIRE_ML=1`) — падают.

mod common;

use std::path::PathBuf;
use std::process::Command;

use axum::http::{Method, StatusCode};
use common::api::{Api, api};
use common::ml::{MINILM, READY, external, have, ml, until, vector_ready};
use common::qdrant::{TestQdrant, free_port, qdrant_bin};
use serde_json::{Value, json};
use zam_core::Id;
use zam_index::VectorFilter;
use zam_server::config::{QdrantMode, ServerConfig};
use zam_store::{chunks_for_index, claim_jobs, enqueue_upserts, fail_job, get_node};

fn uid(a: &Api) -> Id {
    a.st.admin_id
}

async fn state(a: &Api, node: &str) -> String {
    let db = a.st.users.get(uid(a)).await.unwrap();
    get_node(&db, node.parse().unwrap())
        .await
        .unwrap()
        .index_state
}

async fn chunk_count(a: &Api, node: &str) -> u64 {
    let db = a.st.users.get(uid(a)).await.unwrap();
    chunks_for_index(&db, node.parse().unwrap())
        .await
        .unwrap()
        .len() as u64
}

/// Точки пользователя администратора (все уровни, в том числе папки).
async fn points(a: &Api, prefix: Option<&str>) -> u64 {
    let f = VectorFilter {
        user_id: uid(a),
        space_ids: vec![],
        path_prefix: prefix.map(str::to_string),
        levels: vec![],
    };
    a.st.index.vector.as_ref().unwrap().count(&f).await.unwrap()
}

fn id(v: &Value) -> String {
    v["id"].as_str().unwrap().to_string()
}

const NOTE: &str = "# Сборка\n\nПроект собирается командой cargo build, тесты идут через cargo test.\n\n\
                    ## Окружение\n\nПеременные задаёт скрипт dev-env.\n";

async fn status(a: &Api) -> Value {
    let r = a.t(Method::GET, "/api/v1/index/status", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

/// Статус администратора без `X-Zam-User`: все пользователи, в том числе reindex всех баз
/// (токену виден только reindex своего пользователя).
async fn adm_status(a: &Api) -> Value {
    let r = a.adm(Method::GET, "/api/v1/index/status", None, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

/// Две неудачи подряд одного узла — два задания `failed`: в `failed_items` различаются по
/// `id` задания (ключ строк WebUI), у каждого — пространство и путь узла (переход к заметке).
/// Без моделей воркер не запущен, задания выдаёт и проваливает сам тест.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_items_have_job_id_space_and_path() {
    let a = api("ia-failed", |_| {}).await;
    let n = a.note("knowledge", "Папка/сбой", NOTE).await;
    let node: Id = id(&n).parse().unwrap();
    let db = a.st.users.get(uid(&a)).await.unwrap();
    let mut now = jiff::Timestamp::now();
    // два захода по MAX_ATTEMPTS неудач; второе задание — новое после первого `failed`
    for round in 0..2 {
        if round == 1 {
            assert_eq!(enqueue_upserts(&db, vec![node]).await.unwrap(), 1);
        }
        for _ in 0..zam_store::jobs::MAX_ATTEMPTS {
            now += jiff::SignedDuration::from_hours(2);
            let j = claim_jobs(&db, 10, now).await.unwrap().remove(0);
            assert_eq!(j.target_id, node);
            fail_job(&db, j.id, j.claim_gen, "Qdrant: отказ", now)
                .await
                .unwrap();
        }
    }
    let s = adm_status(&a).await;
    let items = s["failed_items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{s}");
    assert_ne!(items[0]["id"], items[1]["id"], "{s}");
    for it in items {
        assert!(it["id"].is_i64(), "{it}");
        assert_eq!(it["node_id"], node.to_string());
        assert_eq!(it["path"], "Папка/сбой");
        assert_eq!(it["space_id"], n["space_id"]);
        assert_eq!(it["error"], "Qdrant: отказ");
    }
    // причина деградации — кодом и текстом для человека
    // (без ORT рядом с тестом — `no_runtime`, с ним — `no_model`)
    assert!(
        s["vector"]["text"]
            .as_str()
            .unwrap()
            .starts_with("модели не загружены"),
        "{s}"
    );
}

/// Без моделей: векторный канал выключен с причиной `no_model` и подсказкой команды;
/// Qdrant не запущен; токен видит только свою очередь, администратор — свою, чужую
/// (`X-Zam-User`) и сумму; reindex по токену — 403, без воркера — 409.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn status_without_models_and_queue_visibility() {
    if !have(MINILM) {
        return;
    }
    let empty = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("ia-no-models");
    let _ = std::fs::remove_dir_all(&empty);
    std::fs::create_dir_all(&empty).unwrap();
    let a = api("ia-nomodel", |c| {
        ml(c, MINILM);
        c.ml.models_dir = Some(empty.clone());
        c.ml.embed_model = None;
    })
    .await;
    // у администратора 2 заметки в очереди, у второго пользователя — 1
    a.note("knowledge", "a1", NOTE).await;
    a.note("knowledge", "a2", NOTE).await;
    let (bob, bob_token) = a.new_user("bob").await;
    let r = a
        .tok(
            &bob_token,
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": "knowledge", "path": "b1", "kind": "note", "content": NOTE})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());

    let s = status(&a).await;
    assert_eq!(s["vector"]["available"], false);
    assert_eq!(s["vector"]["reason"], "no_model");
    assert!(
        s["vector"]["hint"]
            .as_str()
            .unwrap()
            .contains("zam-server models fetch"),
        "{s}"
    );
    assert_eq!(s["model"]["id"], Value::Null);
    assert_eq!(s["qdrant"]["state"], "off");
    assert_eq!(s["qdrant"]["points"], Value::Null);
    assert_eq!(s["qdrant"]["rss_bytes"], Value::Null);
    assert_eq!(s["queue"]["queued"], 2);
    assert!(s["queue"]["oldest"].is_string());
    assert_eq!(s["failed_items"], json!([]));

    // токен второго пользователя — только его очередь (X-Zam-User по токену игнорируется)
    let r = a
        .tok_h(
            &bob_token,
            Method::GET,
            "/api/v1/index/status",
            None,
            &[("x-zam-user", &a.admin_id())],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["queue"]["queued"], 1);
    // администратор: чужая очередь по X-Zam-User, без заголовка — сумма по всем
    let r = a
        .adm(Method::GET, "/api/v1/index/status", None, Some(&bob))
        .await;
    assert_eq!(r.json()["queue"]["queued"], 1);
    let r = a.adm(Method::GET, "/api/v1/index/status", None, None).await;
    assert_eq!(r.json()["queue"]["queued"], 3);

    // reindex: токен — 403; администратор без воркера — 409 с подсказкой про CLI
    let r = a
        .tok(
            &bob_token,
            Method::POST,
            "/api/v1/admin/reindex",
            Some(json!({})),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = a
        .adm(Method::POST, "/api/v1/admin/reindex", Some(json!({})), None)
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert!(r.text().contains("zam-server reindex"), "{}", r.text());
    a.st.shutdown().await;
}

/// С MiniLM и Qdrant: канал включён, точки есть. Коллекция удалена → `POST /admin/reindex`
/// восстанавливает точки из кэша, модель не вызывается; аудит `index.reindex`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn status_ready_and_admin_reindex_restores_points_without_inference() {
    if !have(MINILM) {
        return;
    }
    let q = TestQdrant::start("ia-reidx");
    let a = api("ia-reidx", |c| {
        ml(c, MINILM);
        external(c, &q);
    })
    .await;
    vector_ready(&a).await;
    let n1 = id(&a.note("knowledge", "docs/n1", NOTE).await);
    // Rules: запись агентом закрыта (rules.agent_write = false) — от администратора
    let r = a
        .adm(
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": "rules", "path": "r1", "kind": "note",
                        "content": "Правило: перед коммитом — тесты.\n"})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let n2 = id(&r.json());
    until("indexed", READY, async || {
        state(&a, &n1).await == "indexed" && state(&a, &n2).await == "indexed"
    })
    .await;
    let want = chunk_count(&a, &n1).await + chunk_count(&a, &n2).await;
    until("точки и папка", READY, async || {
        points(&a, None).await == want + 1
    })
    .await;
    let s = status(&a).await;
    assert_eq!(
        s["vector"],
        json!({"available": true, "reason": null, "text": null, "hint": null})
    );
    assert_eq!(s["device"]["kind"], "cpu");
    assert!(s["model"]["id"].as_str().unwrap().starts_with(MINILM));
    assert_eq!(s["model"]["dim"], 384);
    assert_eq!(s["model"]["collection_model"], s["model"]["id"]);
    assert_eq!(s["qdrant"]["state"], "external");
    assert_eq!(s["qdrant"]["points"], want + 1);
    assert_eq!(s["qdrant"]["restarts"], 0);
    assert_eq!(s["queue"]["queued"], 0);
    assert_eq!(s["reindex"]["last"], Value::Null);

    // коллекция пропала (удалена вручную); reindex из кэша
    let before = a.st.index.worker.embedded_texts();
    a.st.index
        .vector
        .as_ref()
        .unwrap()
        .drop_all()
        .await
        .unwrap();
    // неверные запросы
    let bad = a
        .adm(
            Method::POST,
            "/api/v1/admin/reindex",
            Some(json!({"space": "rules"})),
            None,
        )
        .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST, "{}", bad.text());
    let bad = a
        .adm(
            Method::POST,
            "/api/v1/admin/reindex",
            Some(json!({"userr": "x"})),
            None,
        )
        .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST, "{}", bad.text());
    let r = a
        .adm(Method::POST, "/api/v1/admin/reindex", None, None)
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    assert_eq!(r.json()["job_count"], 2);
    assert_eq!(r.json()["full"], false);
    let t = std::time::Instant::now();
    loop {
        let s = adm_status(&a).await;
        if s["reindex"]["last"].is_object() && s["reindex"]["running"] == false {
            break;
        }
        assert!(t.elapsed() < READY, "reindex не завершён: {s}");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    // reindex всех баз токену не виден (только reindex своего пользователя)
    assert_eq!(status(&a).await["reindex"]["last"], Value::Null);
    let s = adm_status(&a).await;
    let res = &s["reindex"]["last"]["result"];
    assert_eq!(res["nodes"], 2, "{s}");
    assert_eq!(res["points"], want);
    assert_eq!(res["folders"], 1);
    assert_eq!(res["misses"], 0);
    assert_eq!(points(&a, None).await, want + 1);
    assert_eq!(
        a.st.index.worker.embedded_texts(),
        before,
        "reindex из кэша без инференса"
    );
    until("канал включён", READY, async || {
        a.st.index.vector_available().is_ok()
    })
    .await;
    let audit = a.st.system.list_audit(50, None).await.unwrap();
    let ev = audit
        .iter()
        .find(|r| r.action == "index.reindex")
        .expect("аудит index.reindex");
    assert_eq!(ev.actor, "admin");
    assert!(audit.iter().any(|r| r.action == "index.reindex_done"));

    // пространство одного пользователя, полный режим: задания, инференс промахов
    let r = a
        .adm(
            Method::POST,
            "/api/v1/admin/reindex",
            Some(json!({"user": a.admin_id(), "space": "rules", "full": true})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    assert_eq!(r.json()["job_count"], 1);
    until("полный reindex", READY, async || {
        adm_status(&a).await["reindex"]["last"]["full"] == true
    })
    .await;
    until("r1 indexed", READY, async || {
        state(&a, &n2).await == "indexed"
    })
    .await;
    assert_eq!(points(&a, Some("r1")).await, chunk_count(&a, &n2).await);
    a.st.shutdown().await;
}

fn zam_server(args: &[&str]) -> std::process::Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_zam-server"));
    c.args(args);
    for (k, _) in std::env::vars().filter(|(k, _)| k.starts_with("ZAM_")) {
        c.env_remove(k);
    }
    c.output().expect("запуск zam-server")
}

/// Управляемый Qdrant: статус с RSS процесса и версией; `zam-server reindex` на
/// работающем сервере отказывает (лок каталога), на остановленном — восстанавливает
/// точки из кэша без инференса.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn managed_status_rss_and_cli_reindex_on_stopped_server() {
    if !have(MINILM) {
        return;
    }
    let (http, grpc) = (free_port(), free_port());
    let dir = common::tmp("ia-cli");
    let mut c: ServerConfig = common::cfg_in(dir.join("d"));
    ml(&mut c, MINILM);
    c.qdrant.mode = QdrantMode::Managed;
    c.qdrant.binary = Some(qdrant_bin());
    c.qdrant.http_port = http;
    c.qdrant.grpc_port = grpc;
    let conf = dir.join("zam.toml");
    std::fs::write(&conf, toml::to_string(&c).unwrap()).unwrap();
    let conf_arg = conf.to_str().unwrap().to_string();

    let a = common::api::api_with(c.clone()).await;
    vector_ready(&a).await;
    let n = id(&a.note("knowledge", "keep/n", NOTE).await);
    until("n indexed", READY, async || {
        state(&a, &n).await == "indexed"
    })
    .await;
    let s = status(&a).await;
    assert_eq!(s["qdrant"]["state"], "ready");
    assert_eq!(s["qdrant"]["version"], "1.19.2");
    assert!(
        s["qdrant"]["rss_bytes"].as_u64().unwrap() > 1_000_000,
        "{s}"
    );

    // сервер работает: каталог данных занят
    let out = zam_server(&["reindex", "--config", &conf_arg]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("занят") && err.contains("admin/reindex"),
        "{err}"
    );
    let pts = points(&a, Some("keep")).await;
    assert!(pts > 0);
    a.st.shutdown().await;
    drop(a);

    // хранилище стёрто вручную, у второго пользователя база повреждена: на остановленном
    // сервере reindex всех баз из кэша пропускает её и называет в отчёте
    let qdir = dir.join("d").join("qdrant").join("storage");
    let _ = std::fs::remove_dir_all(&qdir);
    let bad = {
        let dd = zam_store::DataDir::open(&dir.join("d")).unwrap();
        let sys = zam_store::SystemStore::open(&dd).unwrap();
        sys.create_user("bad", false).await.unwrap().id
    };
    std::fs::write(
        dir.join("d").join("users").join(format!("{bad}.db")),
        b"not a sqlite database at all",
    )
    .unwrap();
    let out = zam_server(&["reindex", "--config", &conf_arg]);
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "{text}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("из кэша") && text.contains("заметок из кэша 1"),
        "{text}"
    );
    assert!(
        text.contains(&format!("базы пользователя {bad}"))
            || text.contains(&format!("база пользователя {bad}")),
        "{text}"
    );
    // явно указанная повреждённая база — ошибка команды
    let out = zam_server(&["reindex", "--config", &conf_arg, "--user", &bad.to_string()]);
    assert!(!out.status.success());

    // сервер снова: точки на месте сразу, модель не вызывалась
    let b = common::api::api_with(c).await;
    until("точки", READY, async || {
        b.st.index.vector_available().is_ok() && points(&b, Some("keep")).await == pts
    })
    .await;
    assert_eq!(b.st.index.worker.embedded_texts(), 0);
    let audit = b.st.system.list_audit(50, None).await.unwrap();
    assert!(
        audit
            .iter()
            .any(|r| r.action == "index.reindex" && r.actor == "cli")
    );
    // исключение из CLI пережило перезапуск: сервер видит базу в статусе
    let s = b
        .adm(Method::GET, "/api/v1/index/status", None, None)
        .await
        .json();
    assert_eq!(s["rebuild_skipped"][0]["user_id"], bad.to_string(), "{s}");
    // пространство без пользователя — ошибка разбора аргументов
    let out = zam_server(&["reindex", "--config", &conf_arg, "--space", "rules"]);
    assert!(!out.status.success());
    b.st.shutdown().await;
}
