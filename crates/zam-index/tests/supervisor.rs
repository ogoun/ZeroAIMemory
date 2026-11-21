//! Интеграционные тесты супервизора нативного Qdrant (задача 6 M2, §4.3) на настоящем
//! бинарнике `target/deps/<платформа>/qdrant/` (`cargo xtask fetch-deps`).
//!
//! Каждый тест — свой каталог данных под `CARGO_TARGET_TMPDIR` и свои порты 127.0.0.1. Данные
//! успешного теста удаляются ([`TestDir`]); тест, пишущий в Qdrant, сначала проверяет место на
//! диске ([`common::disk::enough_disk`]: мало — `SKIP`, в CI и с `ZAM_REQUIRE_DISK=1` — падение).
//! Тест, который поднимает Qdrant (супервизор, проба), первой строкой берёт слот запуска
//! [`common::qdrant::Slot`] на весь тест — предел `ZAM_TEST_QDRANT_MAX`, как у `TestQdrant`: при
//! полном параллелизме без него шло до 17 процессов сразу, и тесты со сроками («Ready (повтор)»,
//! «WAL не подтверждён») падали от взаимной нагрузки (M6.6, задача 12, ревью). Слот не кладётся
//! в [`TestDir`] и `FlushRun`: четыре каталога `flush_run` заняли бы четыре слота.

mod common;

use std::io::BufRead as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::disk::{TestDir, enough_disk};
use common::qdrant::{TestQdrant, free_port, qdrant_bin};
use tokio::sync::watch;
use zam_index::supervisor::{
    ManagedConfig, QDRANT_VERSION, QdrantState, Supervisor, SupervisorConfig, inspect,
    is_qdrant_process, pid_alive, reset_storage,
};
use zam_index::{IndexError, QdrantIndex, VectorIndex};

/// Свежий каталог данных теста (`zam-index-sup/<имя>-<pid>`; удаляется после успешного теста).
fn data_dir(name: &str) -> TestDir {
    TestDir::new("zam-index-sup", name)
}

/// Порог места: `ZAM_TEST_MIN_FREE_MB` переопределяет 4096 МБ; при нехватке — текст `SKIP` с
/// местом, порогом, путём и советом очистки; места хватает — пропуска нет.
#[test]
fn require_free_disk_threshold() {
    use common::disk::{MIN_FREE_MB, disk_shortage, pid_before_seq, pid_suffix, threshold_mb};
    assert_eq!(MIN_FREE_MB, 4096);
    assert_eq!(threshold_mb(None), MIN_FREE_MB);
    assert_eq!(threshold_mb(Some("99999999")), 99_999_999);
    assert_eq!(threshold_mb(Some(" 512 ")), 512);
    assert_eq!(threshold_mb(Some("много")), MIN_FREE_MB);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let msg = disk_shortage(dir, threshold_mb(Some("99999999")), "t").expect("пропуск");
    assert!(msg.starts_with("SKIP t: свободно "), "{msg}");
    assert!(msg.contains(" МБ < 99999999 МБ на "), "{msg}");
    assert!(msg.contains(&dir.display().to_string()), "{msg}");
    assert!(
        msg.ends_with("освободите место (cargo xtask clean-test-tmp)"),
        "{msg}"
    );
    assert_eq!(disk_shortage(dir, 0, "t"), None);
    // имена каталогов прогонов: `<имя>-<pid>`, в имени теста тоже бывает «-»
    assert_eq!(pid_suffix("flushwait-full-4242"), Some(4242));
    assert_eq!(pid_suffix("ready-17"), Some(17));
    assert_eq!(pid_suffix("ready"), None);
    assert_eq!(pid_suffix("ready-x1"), None);
    // каталоги TestQdrant: `<имя>-<pid>-<номер>`
    assert_eq!(pid_before_seq("worker-gc-245184-1"), Some(245184));
    assert_eq!(pid_before_seq("foreign-17-0"), Some(17));
    assert_eq!(pid_before_seq("ready-17"), None);
    assert_eq!(pid_before_seq("ready-17-x"), None);
}

/// [`TestDir`]: первое создание под базой удаляет каталоги завершившихся процессов, каталоги
/// живого (своего) процесса и без `pid` в имени не трогает; после успешного теста каталог
/// удаляется.
#[test]
fn test_dir_sweeps_dead_runs_and_removes_itself() {
    const BASE: &str = "zam-index-sweep-probe";
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(BASE);
    let _ = std::fs::remove_dir_all(&root);
    // pid, которого нет ни в Windows, ни в Linux
    let dead = root.join(format!("old-run-{}", u32::MAX - 1));
    let mine = root.join(format!("other-{}", std::process::id()));
    let plain = root.join("no-pid-here");
    for d in [&dead, &mine, &plain] {
        std::fs::create_dir_all(d.join("storage")).unwrap();
        std::fs::write(d.join("storage").join("data"), b"x").unwrap();
    }
    let t = TestDir::new(BASE, "probe");
    assert!(!dead.exists(), "каталог мёртвого прогона остался");
    assert!(mine.exists() && plain.exists(), "лишнее удалено");
    assert!(t.is_dir() && t.starts_with(&root));
    let path = t.path().to_path_buf();
    drop(t);
    assert!(!path.exists(), "каталог успешного теста не удалён");
    let _ = std::fs::remove_dir_all(&root);
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

/// Предел первой готовности (старт Qdrant на пустом каталоге данных) и переходов супервизора
/// (`Restarting`, `Broken`).
const READY_LIMIT: Duration = Duration::from_secs(10);
/// Предел повторной готовности — после перезапуска Qdrant: рестарт супервизором после смерти
/// или паники процесса, новый `Supervisor::start` на каталоге прежнего запуска. Тесты с этим
/// пределом проверяют не скорость старта, а то, что готовность наступает: предел лишь ловит
/// зависание. Полный прогон на загруженной машине (две параллельные сборки, `-j 2`; финальное
/// ревью M6.6) не уложил повторную готовность `config_renders_flush_interval_and_wal`
/// (коллекция с payload-индексами) в 10 с — состояние оставалось `Starting`; под общим прогоном
/// на HDD Qdrant медлит и в других местах (один payload-индекс пустой коллекции создаётся
/// 17–35 с, задача 12 M6.6). 60 с — как у готовности `TestQdrant` (`common::qdrant`, «данные
/// проекта на HDD»). Время повторного старта с коллекцией сторожит отдельный [`RESTART_LIMIT`].
const READY_AGAIN_LIMIT: Duration = Duration::from_secs(60);
/// Предел повторного старта сразу после создания коллекции в
/// `start_ready_restart_with_collection_and_clean_shutdown`. Коллекция пишется клиентом без
/// отметки супервизора (`QdrantIndex::connect`, не `connect_index`), поэтому остановка не
/// ждёт подтверждения WAL, и старт повторяет WAL создания коллекции с синхронным сбросом
/// сегментов: для Windows (`TerminateProcess`) это сценарий жёсткой остановки после записи,
/// целью §17 (5 с) не покрытый. Замеры на HDD (задача 20 M3): одиночный прогон бинарника —
/// 6,5–7,6 с (4 прогона); в задачах 19 и 19b тот же код укладывался в 10 с и в полном
/// `cargo test --workspace`, в задаче 20 два полных прогона подряд превысили 10 с, причина
/// не установлена. Малый запас при 10 с, возможно, объясняется 12 payload-индексами против 6
/// в M2 — гипотеза, не замер. 20 с — около 2,6× от наблюдаемых 6,5–7,6 с: сторож регрессии
/// остаётся, шум HDD тест не роняет. Время печатается (`--nocapture`).
const RESTART_LIMIT: Duration = Duration::from_secs(20);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_ready_restart_with_collection_and_clean_shutdown() {
    let _slot = common::qdrant::Slot::take();
    if !enough_disk("start_ready_restart_with_collection_and_clean_shutdown") {
        return;
    }
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

    // Повторный старт с существующей коллекцией (время печатается; предел — RESTART_LIMIT).
    let t1 = Instant::now();
    let sup = Supervisor::start(SupervisorConfig::Managed(cfg.clone()), &dir)
        .await
        .unwrap();
    let mut rx = sup.state();
    wait_state(&mut rx, RESTART_LIMIT, "Ready (повтор)", is_ready).await;
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
    let _slot = common::qdrant::Slot::take();
    let dir = data_dir("crash");
    let sup = Supervisor::start(SupervisorConfig::Managed(managed()), &dir)
        .await
        .unwrap();
    let mut rx = sup.state();
    let mut pids = Vec::new();
    for i in 1..=5u32 {
        // первая готовность — пустой каталог; дальше — после рестарта супервизором (окно падений
        // 10 мин, четыре ожидания по READY_AGAIN_LIMIT в него укладываются)
        let limit = if i == 1 {
            READY_LIMIT
        } else {
            READY_AGAIN_LIMIT
        };
        wait_state(&mut rx, limit, "Ready", is_ready).await;
        let pid = sup.pid().expect("pid");
        // Windows сразу отдаёт освободившийся PID новому процессу (CI 41605984312): там
        // перезапуск доказывают `Restarting { attempt }` и новый `Ready`, а не новый PID.
        if cfg!(not(windows)) {
            assert!(!pids.contains(&pid), "pid не сменился после перезапуска");
        }
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

/// Смерть процесса видна [`Supervisor::process_alive`] сразу, без ожидания супервизора:
/// поиск по ней уходит в FTS-only, не дожидаясь отказа соединения (на Windows — ~2 с на
/// попытку; задача 15 M2). Перезапуск отложен на 3 с, чтобы новый процесс не успел
/// подняться за время проверки.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn process_alive_drops_right_after_kill() {
    let _slot = common::qdrant::Slot::take();
    let dir = data_dir("alive");
    let mut cfg = managed();
    cfg.restart.initial = Duration::from_secs(3);
    cfg.restart.max = Duration::from_secs(3);
    let sup = Supervisor::start(SupervisorConfig::Managed(cfg), &dir)
        .await
        .unwrap();
    let mut rx = sup.state();
    wait_state(&mut rx, READY_LIMIT, "Ready", is_ready).await;
    assert!(sup.process_alive());
    let pid = sup.pid().expect("pid");
    let t = Instant::now();
    kill_external(pid);
    while sup.process_alive() {
        assert!(
            t.elapsed() < Duration::from_secs(1),
            "process_alive() = true через {:?} после убийства",
            t.elapsed()
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    eprintln!("процесс мёртв для process_alive через {:?}", t.elapsed());
    // супервизор потом всё равно перезапускает Qdrant
    wait_state(&mut rx, READY_LIMIT, "Restarting", |s| {
        matches!(s, QdrantState::Restarting { .. })
    })
    .await;
    wait_state(&mut rx, READY_AGAIN_LIMIT, "Ready (повтор)", is_ready).await;
    assert!(sup.process_alive());
    assert_ne!(sup.pid(), Some(pid));
    sup.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn old_stamp_wipes_storage_and_sets_rebuilt() {
    let _slot = common::qdrant::Slot::take();
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
    let _slot = common::qdrant::Slot::take();
    if !enough_disk("storage_panic_is_detected_wiped_and_restarted_once") {
        return;
    }
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
    let st = wait_state(&mut rx, READY_AGAIN_LIMIT, "Ready после wipe", is_ready).await;
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
    let _slot = common::qdrant::Slot::take();
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
    let _slot = common::qdrant::Slot::take();
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
    // повторный старт на данных, которые писали прежний Qdrant и сирота
    wait_state(&mut rx, READY_AGAIN_LIMIT, "Ready (повтор)", is_ready).await;
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

/// reset не стирает хранилище из-под живого Qdrant: процесс без pid-файла (его нельзя
/// опознать как нашего сироту) держит наш порт и отвечает как Qdrant.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reset_refuses_while_live_qdrant_holds_our_port() {
    let _slot = common::qdrant::Slot::take();
    let dir = data_dir("reset-live");
    let cfg = managed();
    let sup = Supervisor::start(SupervisorConfig::Managed(cfg.clone()), &dir)
        .await
        .unwrap();
    let mut rx = sup.state();
    wait_state(&mut rx, READY_LIMIT, "Ready", is_ready).await;
    sup.shutdown().await;

    let qdir = dir.join("qdrant");
    // процесс завершается и при панике теста (иначе он держит канал вывода cargo)
    struct Kill(std::process::Child);
    impl Drop for Kill {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let live = Kill(
        Command::new(qdrant_bin())
            .args(["--config-path", "config/zam.yaml", "--disable-telemetry"])
            .current_dir(&qdir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_listening(cfg.http_port);
    // pid-файла нет: сирота не опознан, но процесс жив на нашем порту
    let _ = std::fs::remove_file(qdir.join("qdrant.pid"));
    let err = reset_storage(&dir, &cfg).unwrap_err();
    assert!(
        matches!(&err, IndexError::Process(m) if m.contains("не стёрто")),
        "{err:?}"
    );
    assert!(pid_alive(live.0.id()), "живой Qdrant завершён");
    let st = inspect(&dir, cfg.http_port).await;
    assert!(st.storage_present && !st.reindex_pending, "{st:?}");

    drop(live);
    reset_storage(&dir, &cfg).unwrap();
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
    // один слот на два процесса: `TestQdrant` уже держит слот запуска, второй
    // (`Slot::take` для управляемого Qdrant) при `ZAM_TEST_QDRANT_MAX=1` заблокировал бы тест
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
    // у внешнего Qdrant процесса нет: доступность — только по запросам
    assert!(ext.process_alive());
    assert!(!format!("{ext:?}").contains("\"k\""));
    ext.shutdown().await;
}

/// «Родитель убит» (Review Focus 4): проба держит супервизор, тест убивает её жёстко
/// (`TerminateProcess` / SIGKILL), Qdrant должен исчезнуть за ≤ 5 с — Job Object на
/// Windows, PDEATHSIG на Linux.
#[test]
fn qdrant_dies_with_killed_parent() {
    let _slot = common::qdrant::Slot::take();
    let dir = data_dir("parent");
    let cfg = managed();
    let mut probe = Command::new(env!("CARGO_BIN_EXE_kill_parent_probe"))
        .arg(dir.path())
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

// ------------------------------------------------- задача 19 M3: WAL и сброс Qdrant

/// Конфигурация коллекции на диске Qdrant (`storage/collections/<имя>/config.json`).
fn collection_config(dir: &Path) -> serde_json::Value {
    let f = dir
        .join("qdrant")
        .join("storage")
        .join("collections")
        .join(zam_index::COLLECTION)
        .join("config.json");
    let text = std::fs::read_to_string(&f).unwrap_or_else(|e| panic!("{}: {e}", f.display()));
    serde_json::from_str(&text).unwrap()
}

/// `zam.yaml` задаёт умолчания WAL и сброса для новых коллекций; клиент супервизора
/// создаёт коллекцию с ними явно, а у существующей коллекции меняет `flush_interval_sec`
/// (`update_collection`). Размер блока WAL Qdrant 1.19 у существующей коллекции не меняет
/// (в `UpdateCollection` нет `wal_config`) — он остаётся прежним до пересоздания.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_renders_flush_interval_and_wal() {
    let _slot = common::qdrant::Slot::take();
    if !enough_disk("config_renders_flush_interval_and_wal") {
        return;
    }
    let dir = data_dir("walcfg");
    let mut cfg = managed();
    cfg.flush_interval_sec = 1;
    cfg.wal_capacity_mb = 4;
    let sup = Supervisor::start(SupervisorConfig::Managed(cfg.clone()), &dir)
        .await
        .unwrap();
    let mut rx = sup.state();
    wait_state(&mut rx, READY_LIMIT, "Ready", is_ready).await;
    let yaml = std::fs::read_to_string(dir.join("qdrant").join("config").join("zam.yaml")).unwrap();
    for needle in [
        "  wal:\n    wal_capacity_mb: 4\n",
        "  optimizers:\n    flush_interval_sec: 1\n",
    ] {
        assert!(yaml.contains(needle), "в zam.yaml нет `{needle}`:\n{yaml}");
    }
    let idx = sup.connect_index().unwrap();
    idx.ensure(common::cases::MODEL, common::cases::DIM)
        .await
        .unwrap();
    let c = collection_config(&dir);
    assert_eq!(c["wal_config"]["wal_capacity_mb"], 4, "{c}");
    assert_eq!(c["optimizer_config"]["flush_interval_sec"], 1, "{c}");
    sup.shutdown().await;

    // существующая коллекция: интервал сброса обновляется, размер блока WAL — нет
    cfg.flush_interval_sec = 2;
    cfg.wal_capacity_mb = 8;
    let sup = Supervisor::start(SupervisorConfig::Managed(cfg), &dir)
        .await
        .unwrap();
    let mut rx = sup.state();
    // повторный старт с коллекцией и её payload-индексами: предел — READY_AGAIN_LIMIT, тест
    // проверяет конфигурацию, а не скорость старта (финальное ревью M6.6: 10 с не хватило)
    wait_state(&mut rx, READY_AGAIN_LIMIT, "Ready (повтор)", is_ready).await;
    let idx = sup.connect_index().unwrap();
    let meta = idx
        .ensure(common::cases::MODEL, common::cases::DIM)
        .await
        .unwrap();
    assert!(!meta.created);
    let c = collection_config(&dir);
    assert_eq!(c["optimizer_config"]["flush_interval_sec"], 2, "{c}");
    assert_eq!(c["wal_config"]["wal_capacity_mb"], 4, "{c}");
    sup.shutdown().await;
}

/// Записывает `batches` пачек точек через клиент супервизора (каждая — операция WAL).
async fn write_batches(idx: &QdrantIndex, batches: u64) {
    let ctx = common::cases::Ctx {
        user: zam_core::Id::new(),
        space: zam_core::Id::new(),
    };
    for b in 0..batches {
        let pts = (0..8)
            .map(|i| {
                let seed = b * 100 + i;
                common::cases::point(
                    &ctx,
                    zam_core::Id::new(),
                    &format!("c{seed}"),
                    seed,
                    "a/b",
                    "chunk",
                )
            })
            .collect();
        idx.upsert(pts).await.unwrap();
    }
}

/// Итог одного прогона [`shutdown_waits_for_flush_bounded`].
struct FlushRun {
    took: Duration,
    wait: Option<zam_index::supervisor::WalWait>,
    last_op: Option<u64>,
    ack: Option<u64>,
    /// Свободное место тома данных после остановки, МБ — для разбора неподтверждённого WAL
    /// (на почти полном диске Qdrant не успевает его подтвердить).
    free_mb: Option<u64>,
    /// Каталог прогона живёт до конца теста: проверки идут после `flush_run`, и при их отказе
    /// данные Qdrant остаются для разбора.
    _dir: TestDir,
}

/// Старт, запись (или нет), пауза `idle`, остановка с пределом `cap`; время остановки и
/// подтверждённая часть WAL после неё. Места на диске мало — `None` (тест пропускается; каждый
/// из четырёх прогонов подряд пишет WAL Qdrant).
async fn flush_run(
    name: &str,
    flush: u64,
    cap: Duration,
    write: bool,
    idle: Duration,
) -> Option<FlushRun> {
    if !enough_disk(&format!("shutdown_waits_for_flush_bounded ({name})")) {
        return None;
    }
    let dir = data_dir(name);
    let mut cfg = managed();
    cfg.flush_interval_sec = flush;
    cfg.shutdown_flush_wait = cap;
    let sup = Supervisor::start(SupervisorConfig::Managed(cfg), &dir)
        .await
        .unwrap();
    let mut rx = sup.state();
    wait_state(&mut rx, READY_LIMIT, "Ready", is_ready).await;
    let idx = sup.connect_index().unwrap();
    idx.ensure(common::cases::MODEL, common::cases::DIM)
        .await
        .unwrap();
    if write {
        write_batches(&idx, 3).await;
    }
    tokio::time::sleep(idle).await;
    let last_op = sup.write_mark().last_op();
    let t = Instant::now();
    let wait = sup.shutdown().await;
    let took = t.elapsed();
    Some(FlushRun {
        took,
        wait,
        last_op,
        ack: zam_index::supervisor::wal_ack_index(&dir.join("qdrant")),
        free_mb: common::disk::free_mb(&dir),
        _dir: dir,
    })
}

/// Остановка после записи ждёт, пока Qdrant подтвердит её в WAL (`first-index` против
/// `operation_id`), но не дольше `shutdown_flush_wait`; подтверждённую запись и нулевой
/// предел — не ждёт.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_waits_for_flush_bounded() {
    let _slot = common::qdrant::Slot::take();
    // интервал сброса 1 с, предел FULL_CAP: дожидается подтверждения раньше предела. На HDD два
    // прохода фонового сброса — 6–10 с (задача 19 M3), но полный прогон на загруженной машине
    // (две параллельные сборки, `-j 2`; финальное ревью M6.6) ждал подтверждения 29,79 с при
    // прежнем пределе 30 с — WAL подтверждён, а проверка «< 29 с» упала. Предел 120 с — около 4×
    // от худшего наблюдения: тест проверяет, что остановка ждёт подтверждения, а не скорость
    // сброса. Запас FULL_MARGIN отличает подтверждение от упора в предел: упёрлась бы остановка
    // в предел — `took` был бы не меньше FULL_CAP.
    const FULL_CAP: Duration = Duration::from_secs(120);
    const FULL_MARGIN: Duration = Duration::from_secs(10);
    let Some(full) = flush_run("flushwait-full", 1, FULL_CAP, true, Duration::ZERO).await else {
        return;
    };
    let op = full.last_op.expect("номер операции из ответа upsert");
    assert!(
        full.ack.is_some_and(|a| a >= op),
        "WAL не подтверждён: ack {:?}, последняя операция {op}; свободно {:?} МБ на {}",
        full.ack,
        full.free_mb,
        env!("CARGO_TARGET_TMPDIR")
    );
    assert!(
        full.took < FULL_CAP - FULL_MARGIN,
        "{:?} — остановка упёрлась в предел {FULL_CAP:?}, а не дождалась подтверждения",
        full.took
    );
    assert_eq!(
        full.wait.map(|w| w.result),
        Some("acked"),
        "{:?}",
        full.wait
    );
    // интервал 5 с, предел 1 с: подтверждения не будет раньше первого сброса — ждёт предел
    let Some(capped) = flush_run(
        "flushwait-cap",
        5,
        Duration::from_secs(1),
        true,
        Duration::ZERO,
    )
    .await
    else {
        return;
    };
    assert!(
        capped.took >= Duration::from_millis(900) && capped.took < Duration::from_secs(6),
        "{:?}",
        capped.took
    );
    let w = capped.wait.expect("ожидание было");
    assert_eq!(w.result, "timeout");
    assert!(
        w.waited >= Duration::from_secs(1) && w.waited < Duration::from_secs(2),
        "{w:?}"
    );
    // предел 0 — вариант выключен
    let Some(off) = flush_run("flushwait-off", 5, Duration::ZERO, true, Duration::ZERO).await
    else {
        return;
    };
    // верхние пределы — с запасом на остановку Qdrant по SIGTERM (Linux: начатый проход
    // сброса доходит до конца); проверяется, что ожидания нет, а не скорость выхода
    assert!(off.took < Duration::from_secs(5), "{:?}", off.took);
    assert_eq!(off.wait, None, "при пределе 0 ожидания нет");
    // запись давно подтверждена — не ждёт
    let Some(idle) = flush_run(
        "flushwait-idle",
        1,
        Duration::from_secs(6),
        true,
        Duration::from_secs(20),
    )
    .await
    else {
        return;
    };
    assert!(
        idle.ack >= idle.last_op,
        "{:?} < {:?}",
        idle.ack,
        idle.last_op
    );
    assert!(idle.took < Duration::from_secs(5), "{:?}", idle.took);
    let w = idle.wait.expect("запись была — проверка подтверждения");
    assert_eq!(w.result, "acked");
    assert!(
        w.waited < Duration::from_millis(200),
        "подтверждённое не ждут: {w:?}"
    );
    eprintln!(
        "FLUSH_WAIT full={:?} (op {op}, ack {:?}) capped={:?} off={:?} idle={:?}",
        full.took, full.ack, capped.took, off.took, idle.took
    );
}

/// Файл `first-index` WAL коллекции (Qdrant 1.19).
fn first_index(dir: &Path) -> PathBuf {
    dir.join("qdrant")
        .join("storage")
        .join("collections")
        .join(zam_index::COLLECTION)
        .join("0")
        .join("wal")
        .join("first-index")
}

/// Коллекция создана и записана, первого прохода сброса ещё не было: `flush_interval_sec`
/// = 3600 — первый проход сброса через час, файла `first-index` нет (его пишет только
/// `SerdeWal::ack`) при любой скорости диска и нагрузке параллельных тестов (с 60 с при
/// параллельном прогоне на HDD подтверждение успевало). Супервизор с пределом `cap`. Места на
/// диске мало — `None` (тест пропускается).
async fn unacked(test: &str, name: &str, cap: Duration) -> Option<(TestDir, Supervisor)> {
    if !enough_disk(test) {
        return None;
    }
    let dir = data_dir(name);
    let mut cfg = managed();
    cfg.flush_interval_sec = 3600;
    cfg.shutdown_flush_wait = cap;
    let sup = Supervisor::start(SupervisorConfig::Managed(cfg), &dir)
        .await
        .unwrap();
    let mut rx = sup.state();
    wait_state(&mut rx, READY_LIMIT, "Ready", is_ready).await;
    let idx = sup.connect_index().unwrap();
    idx.ensure(common::cases::MODEL, common::cases::DIM)
        .await
        .unwrap();
    write_batches(&idx, 2).await;
    Some((dir, sup))
}

/// Файла `first-index` нет — подтверждений ещё не было (задача 19, fix round 2): все записи
/// не подтверждены, `wal_pending` — их число, ожидание при остановке опрашивает файл до
/// предела (`timeout`), а не переходит на паузу по времени.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_first_index_means_nothing_acked() {
    let _slot = common::qdrant::Slot::take();
    let Some((dir, sup)) = unacked(
        "missing_first_index_means_nothing_acked",
        "noack",
        Duration::from_millis(1500),
    )
    .await
    else {
        return;
    };
    assert!(
        !first_index(&dir).exists(),
        "до первого прохода сброса файла нет"
    );
    assert_eq!(
        zam_index::supervisor::wal_ack(&dir.join("qdrant")),
        zam_index::supervisor::WalAck::Missing
    );
    let op = sup.write_mark().last_op().expect("номер операции");
    assert_eq!(sup.wal_pending(), Some(op + 1));
    let w = sup.shutdown().await.expect("ожидание было");
    assert_eq!(w.result, "timeout", "{w:?}");
    assert!(w.waited >= Duration::from_millis(1500), "{w:?}");
}

/// Файл `first-index` есть, но не разбирается (формат другой версии Qdrant): число
/// неподтверждённых неизвестно (`None`), ожидание — пауза по времени (`time`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unreadable_first_index_falls_back_to_time() {
    let _slot = common::qdrant::Slot::take();
    let Some((dir, sup)) = unacked(
        "unreadable_first_index_falls_back_to_time",
        "badack",
        Duration::from_millis(1500),
    )
    .await
    else {
        return;
    };
    let f = first_index(&dir);
    std::fs::write(&f, "не JSON").unwrap();
    assert_eq!(
        zam_index::supervisor::wal_ack(&dir.join("qdrant")),
        zam_index::supervisor::WalAck::Unreadable
    );
    assert_eq!(sup.wal_pending(), None);
    let w = sup.shutdown().await.expect("ожидание было");
    assert_eq!(w.result, "time", "{w:?}");
    // пауза — `2 · 3600 + 1` с, урезанная пределом
    assert!(
        w.waited >= Duration::from_millis(1400) && w.waited < Duration::from_secs(3),
        "{w:?}"
    );
    std::fs::write(&f, "{\"ack_index\":7}").unwrap();
    assert_eq!(
        zam_index::supervisor::wal_ack(&dir.join("qdrant")),
        zam_index::supervisor::WalAck::Index(7)
    );
}
