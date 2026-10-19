//! Интеграционные тесты супервизора нативного Qdrant (задача 6 M2, §4.3) на настоящем
//! бинарнике `target/deps/<платформа>/qdrant/` (`cargo xtask fetch-deps`).
//!
//! Каждый тест — свой каталог данных под `CARGO_TARGET_TMPDIR` и свои порты 127.0.0.1.

mod common;

use std::io::BufRead as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::qdrant::{TestQdrant, free_port, qdrant_bin};
use tokio::sync::watch;
use zam_index::supervisor::{
    ManagedConfig, QDRANT_VERSION, QdrantState, Supervisor, SupervisorConfig, inspect,
    is_qdrant_process, pid_alive, reset_storage,
};
use zam_index::{IndexError, QdrantIndex, VectorIndex};

/// Свежий каталог данных теста.
fn data_dir(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("zam-index-sup")
        .join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("каталог теста");
    d
}

/// Конфигурация с быстрым перезапуском (умолчания 1→60 с слишком долгие для тестов).
fn managed() -> ManagedConfig {
    let bin = qdrant_bin();
    assert!(
        bin.is_file(),
        "нет бинарника Qdrant {}: выполните `cargo xtask fetch-deps`",
        bin.display()
    );
    let mut c = ManagedConfig::new(bin);
    c.http_port = free_port();
    c.grpc_port = free_port();
    c.restart.initial = Duration::from_millis(200);
    c.restart.max = Duration::from_millis(500);
    c
}

/// Ждёт состояние, удовлетворяющее `pred`, не дольше `limit`.
async fn wait_state(
    rx: &mut watch::Receiver<QdrantState>,
    limit: Duration,
    what: &str,
    pred: impl Fn(&QdrantState) -> bool,
) -> QdrantState {
    let res = tokio::time::timeout(limit, async {
        loop {
            {
                let s = rx.borrow_and_update();
                if pred(&s) {
                    return s.clone();
                }
            }
            if rx.changed().await.is_err() {
                return rx.borrow().clone();
            }
        }
    })
    .await;
    match res {
        Ok(s) => {
            assert!(pred(&s), "{what}: канал закрыт в состоянии {s:?}");
            s
        }
        Err(_) => panic!("{what}: нет за {limit:?}, состояние {:?}", *rx.borrow()),
    }
}

fn is_ready(s: &QdrantState) -> bool {
    matches!(s, QdrantState::Ready { .. })
}

/// Жёстко завершает процесс средствами ОС — «извне», мимо супервизора.
fn kill_external(pid: u32) {
    let st = if cfg!(windows) {
        Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
    } else {
        Command::new("kill").args(["-9", &pid.to_string()]).status()
    };
    assert!(
        st.expect("kill").success(),
        "не удалось завершить pid {pid}"
    );
}

/// Ждёт исчезновения процесса.
fn wait_gone(pid: u32, limit: Duration) -> bool {
    let t = Instant::now();
    while t.elapsed() < limit {
        if !pid_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    !pid_alive(pid)
}

const READY_LIMIT: Duration = Duration::from_secs(10);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_ready_restart_with_collection_and_clean_shutdown() {
    let dir = data_dir("ready");
    let cfg = managed();
    let t0 = Instant::now();
    let sup = Supervisor::start(SupervisorConfig::Managed(cfg.clone()), &dir)
        .await
        .unwrap();
    let mut rx = sup.state();
    let st = wait_state(&mut rx, READY_LIMIT, "Ready", is_ready).await;
    let cold = t0.elapsed();
    assert_eq!(
        st,
        QdrantState::Ready {
            version: QDRANT_VERSION.to_string(),
            rebuilt: false,
        }
    );
    assert!(!sup.storage_rebuilt());
    let key = sup.api_key().expect("ключ managed");
    assert_eq!(key.len(), 64);
    assert_eq!(
        sup.grpc_url(),
        format!("http://127.0.0.1:{}", cfg.grpc_port)
    );

    // Конфиг, pid-файл и метка хранилища.
    let qdir = dir.join("qdrant");
    let yaml = std::fs::read_to_string(qdir.join("config").join("zam.yaml")).unwrap();
    for needle in [
        "host: 127.0.0.1",
        "telemetry_disabled: true",
        "enable_cors: false",
        "format: json",
        "max_search_threads: 2",
        "optimizer_cpu_budget: 2",
    ] {
        assert!(yaml.contains(needle), "в zam.yaml нет `{needle}`:\n{yaml}");
    }
    assert!(!yaml.contains(&key), "ключ не должен попадать в конфиг");
    let pid = sup.pid().expect("pid");
    assert!(is_qdrant_process(pid));
    let pid_file = std::fs::read_to_string(qdir.join("qdrant.pid")).unwrap();
    assert_eq!(pid_file.trim(), pid.to_string());
    let stamp = std::fs::read_to_string(qdir.join("zam-storage.json")).unwrap();
    assert!(stamp.contains(QDRANT_VERSION), "{stamp}");

    // Ключ работает, коллекция создаётся.
    let idx = QdrantIndex::connect(&sup.grpc_url(), Some(&key)).unwrap();
    idx.health().await.unwrap();
    idx.ensure("test/model@1", 8).await.unwrap();

    let status = inspect(&dir, cfg.http_port).await;
    assert!(
        status.locked && status.pid_alive && status.ready,
        "{status:?}"
    );
    assert_eq!(status.running_version.as_deref(), Some(QDRANT_VERSION));

    sup.shutdown().await;
    assert!(
        wait_gone(pid, Duration::from_secs(2)),
        "Qdrant жив после shutdown"
    );
    assert!(!qdir.join("qdrant.pid").exists(), "pid-файл не удалён");
    let status = inspect(&dir, cfg.http_port).await;
    assert!(
        !status.locked && !status.ready && status.pid.is_none(),
        "{status:?}"
    );

    // Повторный старт с существующей коллекцией: время до Ready (§17 ≤ 5 с).
    let t1 = Instant::now();
    let sup = Supervisor::start(SupervisorConfig::Managed(cfg.clone()), &dir)
        .await
        .unwrap();
    let mut rx = sup.state();
    wait_state(&mut rx, READY_LIMIT, "Ready (повтор)", is_ready).await;
    let warm = t1.elapsed();
    eprintln!(
        "TIME_TO_READY cold={} ms, with_collection={} ms",
        cold.as_millis(),
        warm.as_millis()
    );
    let idx = QdrantIndex::connect(&sup.grpc_url(), sup.api_key().as_deref()).unwrap();
    let meta = idx.ensure("test/model@1", 8).await.unwrap();
    assert!(!meta.created, "коллекция должна сохраниться");
    // Старый ключ после перезапуска не действует: ключ новый при каждом старте.
    let old = QdrantIndex::connect(&sup.grpc_url(), Some(&key)).unwrap();
    assert!(matches!(old.health().await, Err(IndexError::Auth)));
    sup.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_kill_restarts_and_five_crashes_break() {
    let dir = data_dir("crash");
    let sup = Supervisor::start(SupervisorConfig::Managed(managed()), &dir)
        .await
        .unwrap();
    let mut rx = sup.state();
    let mut pids = Vec::new();
    for i in 1..=5u32 {
        wait_state(&mut rx, READY_LIMIT, "Ready", is_ready).await;
        let pid = sup.pid().expect("pid");
        assert!(!pids.contains(&pid), "pid не сменился после перезапуска");
        pids.push(pid);
        kill_external(pid);
        if i < 5 {
            let st = wait_state(&mut rx, READY_LIMIT, "Restarting", |s| {
                matches!(s, QdrantState::Restarting { .. })
            })
            .await;
            let QdrantState::Restarting { attempt, next_in } = st else {
                unreachable!()
            };
            assert_eq!(attempt, i, "номер попытки");
            assert!(next_in <= Duration::from_millis(500), "{next_in:?}");
        }
    }
    let st = wait_state(&mut rx, READY_LIMIT, "Broken", |s| {
        matches!(s, QdrantState::Broken { .. })
    })
    .await;
    let QdrantState::Broken { reason } = st else {
        unreachable!()
    };
    assert!(reason.contains('5'), "{reason}");
    // Circuit breaker: больше не перезапускает.
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(matches!(*rx.borrow(), QdrantState::Broken { .. }));
    assert!(sup.pid().is_none());
    sup.shutdown().await;
    for p in pids {
        assert!(!pid_alive(p), "процесс {p} остался");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn old_stamp_wipes_storage_and_sets_rebuilt() {
    let dir = data_dir("stamp");
    let qdir = dir.join("qdrant");
    std::fs::create_dir_all(qdir.join("storage")).unwrap();
    std::fs::write(qdir.join("storage").join("marker"), b"x").unwrap();
    std::fs::write(
        qdir.join("zam-storage.json"),
        br#"{"qdrant_version":"1.16.0"}"#,
    )
    .unwrap();
    let cfg = managed();
    let sup = Supervisor::start(SupervisorConfig::Managed(cfg.clone()), &dir)
        .await
        .unwrap();
    assert!(sup.storage_rebuilt());
    assert!(!qdir.join("storage").join("marker").exists());
    let mut rx = sup.state();
    let st = wait_state(&mut rx, READY_LIMIT, "Ready", is_ready).await;
    assert!(
        matches!(st, QdrantState::Ready { rebuilt: true, .. }),
        "{st:?}"
    );
    let stamp = std::fs::read_to_string(qdir.join("zam-storage.json")).unwrap();
    assert!(stamp.contains(QDRANT_VERSION), "{stamp}");
    assert!(inspect(&dir, cfg.http_port).await.reindex_pending);
    // Потребитель (воркер, задача 7) подтверждает постановку reindex.
    sup.ack_storage_rebuilt().unwrap();
    assert!(!sup.storage_rebuilt());
    assert!(matches!(
        *rx.borrow(),
        QdrantState::Ready { rebuilt: false, .. }
    ));
    assert!(!inspect(&dir, cfg.http_port).await.reindex_pending);
    sup.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn storage_panic_is_detected_wiped_and_restarted_once() {
    let dir = data_dir("panic");
    let cfg = managed();
    let sup = Supervisor::start(SupervisorConfig::Managed(cfg.clone()), &dir)
        .await
        .unwrap();
    let mut rx = sup.state();
    wait_state(&mut rx, READY_LIMIT, "Ready", is_ready).await;
    let idx = QdrantIndex::connect(&sup.grpc_url(), sup.api_key().as_deref()).unwrap();
    idx.ensure("test/model@1", 8).await.unwrap();
    sup.shutdown().await;

    // Версия коллекции новее приложения → Qdrant падает с panic на старте.
    let info = dir
        .join("qdrant")
        .join("storage")
        .join("collections")
        .join("zam_chunks")
        .join("version.info");
    assert!(info.is_file(), "нет {}", info.display());
    std::fs::write(&info, "99.0.0").unwrap();

    let sup = Supervisor::start(SupervisorConfig::Managed(cfg), &dir)
        .await
        .unwrap();
    assert!(
        !sup.storage_rebuilt(),
        "метка совместима — до запуска не стираем"
    );
    let mut rx = sup.state();
    let st = wait_state(
        &mut rx,
        Duration::from_secs(20),
        "Ready после wipe",
        is_ready,
    )
    .await;
    assert!(sup.storage_rebuilt(), "детектор panic не сработал");
    assert!(
        matches!(st, QdrantState::Ready { rebuilt: true, .. }),
        "{st:?}"
    );
    let idx = QdrantIndex::connect(&sup.grpc_url(), sup.api_key().as_deref()).unwrap();
    assert!(idx.ensure("test/model@1", 8).await.unwrap().created);
    sup.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_supervisor_on_same_dir_is_lock_error_and_reset_refuses() {
    let dir = data_dir("lock");
    let cfg = managed();
    let a = Supervisor::start(SupervisorConfig::Managed(cfg.clone()), &dir)
        .await
        .unwrap();
    let mut other = cfg.clone();
    other.http_port = free_port();
    other.grpc_port = free_port();
    let err = Supervisor::start(SupervisorConfig::Managed(other), &dir)
        .await
        .unwrap_err();
    assert!(matches!(err, IndexError::Locked(_)), "{err:?}");
    assert!(matches!(
        reset_storage(&dir, &cfg),
        Err(IndexError::Locked(_))
    ));
    a.shutdown().await;
    // Остановлен — reset стирает хранилище и требует reindex.
    reset_storage(&dir, &cfg).unwrap();
    let st = inspect(&dir, cfg.http_port).await;
    assert!(!st.storage_present && st.reindex_pending, "{st:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn orphan_from_pid_file_is_killed() {
    let dir = data_dir("orphan");
    let cfg = managed();
    // Первый запуск генерирует конфиг в <data_dir>/qdrant/config/zam.yaml.
    let sup = Supervisor::start(SupervisorConfig::Managed(cfg.clone()), &dir)
        .await
        .unwrap();
    let mut rx = sup.state();
    wait_state(&mut rx, READY_LIMIT, "Ready", is_ready).await;
    sup.shutdown().await;

    // «Осиротевший» Qdrant: запущен мимо супервизора на тех же портах, pid в pid-файле.
    let qdir = dir.join("qdrant");
    let spawn_orphan = || {
        let child = Command::new(qdrant_bin())
            .args(["--config-path", "config/zam.yaml", "--disable-telemetry"])
            .current_dir(&qdir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        std::fs::write(qdir.join("qdrant.pid"), child.id().to_string()).unwrap();
        wait_listening(cfg.http_port);
        child
    };
    let mut orphan = spawn_orphan();
    let opid = orphan.id();

    let sup = Supervisor::start(SupervisorConfig::Managed(cfg.clone()), &dir)
        .await
        .unwrap();
    // Очистка идёт до возврата start: сирота уже завершён.
    assert!(wait_gone(opid, Duration::from_secs(2)), "сирота жив");
    let _ = orphan.wait();
    let mut rx = sup.state();
    wait_state(&mut rx, READY_LIMIT, "Ready", is_ready).await;
    assert_ne!(sup.pid(), Some(opid));
    sup.shutdown().await;

    // reset при оставшемся сироте (сервер убит): сирота завершается, хранилище стирается.
    let mut orphan = spawn_orphan();
    let opid = orphan.id();
    reset_storage(&dir, &cfg).unwrap();
    assert!(
        wait_gone(opid, Duration::from_secs(2)),
        "сирота пережил reset"
    );
    let _ = orphan.wait();
    let st = inspect(&dir, cfg.http_port).await;
    assert!(!st.storage_present && st.reindex_pending, "{st:?}");
}

/// Ждёт, пока на порту 127.0.0.1 появится слушатель (до 20 с).
fn wait_listening(port: u16) {
    let t = Instant::now();
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(
            t.elapsed() < Duration::from_secs(20),
            "порт {port} не открылся"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Устаревший pid-файл указывает на живой, но чужой Qdrant (другой каталог и порты):
/// его не трогаем, pid-файл просто удаляется.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_pid_file_of_foreign_qdrant_is_not_killed() {
    let foreign = TestQdrant::start("foreign");
    let fpid = foreign.pid();
    let dir = data_dir("foreign");
    let qdir = dir.join("qdrant");
    std::fs::create_dir_all(&qdir).unwrap();
    std::fs::write(qdir.join("qdrant.pid"), fpid.to_string()).unwrap();

    let sup = Supervisor::start(SupervisorConfig::Managed(managed()), &dir)
        .await
        .unwrap();
    let mut rx = sup.state();
    wait_state(&mut rx, READY_LIMIT, "Ready", is_ready).await;
    assert!(pid_alive(fpid), "чужой Qdrant убит");
    let pid_file = std::fs::read_to_string(qdir.join("qdrant.pid")).unwrap();
    assert_eq!(pid_file.trim(), sup.pid().unwrap().to_string());
    assert_ne!(sup.pid(), Some(fpid));
    sup.shutdown().await;
    assert!(pid_alive(fpid), "чужой Qdrant убит при остановке");
    drop(foreign);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_binary_is_broken_and_external_has_no_process() {
    let dir = data_dir("nobin");
    let mut cfg = managed();
    cfg.binary = dir.join("no").join("qdrant.exe");
    let sup = Supervisor::start(SupervisorConfig::Managed(cfg), &dir)
        .await
        .unwrap();
    let mut rx = sup.state();
    let st = wait_state(&mut rx, Duration::from_secs(2), "Broken", |s| {
        matches!(s, QdrantState::Broken { .. })
    })
    .await;
    let QdrantState::Broken { reason } = st else {
        unreachable!()
    };
    assert!(reason.contains("qdrant fetch"), "{reason}");
    sup.shutdown().await;

    let ext = Supervisor::start(
        SupervisorConfig::external("http://127.0.0.1:1", Some("k".into())),
        &dir,
    )
    .await
    .unwrap();
    assert_eq!(*ext.state().borrow(), QdrantState::External);
    assert_eq!(ext.grpc_url(), "http://127.0.0.1:1");
    assert_eq!(ext.api_key().as_deref(), Some("k"));
    assert!(ext.pid().is_none());
    assert!(!format!("{ext:?}").contains("\"k\""));
    ext.shutdown().await;
}

/// «Родитель убит» (Review Focus 4): проба держит супервизор, тест убивает её жёстко
/// (`TerminateProcess` / SIGKILL), Qdrant должен исчезнуть за ≤ 5 с — Job Object на
/// Windows, PDEATHSIG на Linux.
#[test]
fn qdrant_dies_with_killed_parent() {
    let dir = data_dir("parent");
    let cfg = managed();
    let mut probe = Command::new(env!("CARGO_BIN_EXE_kill_parent_probe"))
        .arg(&dir)
        .arg(&cfg.binary)
        .arg(cfg.http_port.to_string())
        .arg(cfg.grpc_port.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let stdout = probe.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Some(p) = line.strip_prefix("QDRANT_PID=") {
                let _ = tx.send(p.trim().parse::<u32>().unwrap());
            }
        }
    });
    let qpid = match rx.recv_timeout(Duration::from_secs(20)) {
        Ok(p) => p,
        Err(e) => {
            let _ = probe.kill();
            panic!("проба не сообщила pid Qdrant: {e}");
        }
    };
    assert!(pid_alive(qpid) && is_qdrant_process(qpid));
    probe.kill().unwrap(); // TerminateProcess / SIGKILL
    let _ = probe.wait();
    assert!(
        wait_gone(qpid, Duration::from_secs(5)),
        "Qdrant {qpid} пережил родителя"
    );
}
