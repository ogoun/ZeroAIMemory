//! `zam-server backup`/`restore` и автокопии (M6 §12б.2–§12б.4, задача 18a).
//!
//! Копия снимается тем же кодом, что команда (`zam_server::backup::backup`), в блокирующем
//! потоке теста — при работающем в процессе сервере или после его остановки. Тесты с моделью
//! и Qdrant (`restore_external_qdrant_*`, `restore_then_serve_*`) без ORT, MiniLM или Qdrant
//! пропускаются; в CI (`ZAM_REQUIRE_ML=1`) — падают.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::http::{Method, StatusCode};
use common::api::{Api, api, api_with};
use serde_json::{Value, json};
use zam_core::Id;
use zam_server::backup::schedule::{Action, tick};
use zam_server::backup::{
    BackupError, BackupOpts, BackupReport, MANIFEST_FILE, Manifest, RestoreOpts, VectorPlan,
    backup, restore, test_hooks,
};
use zam_server::config::{BackupSchedule, BackupSection, ServerConfig};
use zam_store::{DataDir, Db, SystemStore, USER_MIGRATIONS};

/// Тесты, сверяющие отпечаток нарезки процесса (`setup_chunker` меняет его глобально), — по
/// одному: иначе сервер с моделью в соседнем тесте подменил бы счётчик токенов посреди сверки.
static CHUNKER: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn cfg_of(a: &Api) -> ServerConfig {
    (*a.st.cfg).clone()
}

fn uid(a: &Api) -> Id {
    a.st.admin_id
}

async fn run_backup(cfg: &ServerConfig, opts: BackupOpts) -> Result<BackupReport, BackupError> {
    let c = cfg.clone();
    tokio::task::spawn_blocking(move || backup(&c, &opts))
        .await
        .unwrap()
}

/// Остановка сервера теста: после неё лок каталога свободен, базы закрыты.
async fn stop(a: Api) {
    a.st.shutdown().await;
    drop(a);
    // фоновые задачи открытия баз отпускают `Db` чуть позже
    tokio::time::sleep(Duration::from_millis(300)).await;
}

const NOTE: &str = "# Сборка\n\nПроект собирается командой cargo build.\n";

/// Сервер с заметками и двумя вложениями (блобы) у пользователя администратора.
async fn seeded(name: &str) -> (Api, Vec<String>) {
    let a = api(name, |_| {}).await;
    let n = a.note("knowledge", "docs/a", NOTE).await;
    a.note("knowledge", "docs/b", "# Б\n\nВторая заметка.\n")
        .await;
    let node = n["id"].as_str().unwrap().to_string();
    let mut shas = Vec::new();
    for (f, len) in [("a.bin", 3000usize), ("b.bin", 5000)] {
        let data: Vec<u8> = (0..len).map(|i| (i * 31 % 251) as u8).collect();
        let j = common::attach::upload(&a, &a.token, &node, f, &data).await;
        shas.push(j["sha256"].as_str().unwrap().to_string());
    }
    (a, shas)
}

fn read_manifest(dir: &Path) -> Manifest {
    serde_json::from_str(&std::fs::read_to_string(dir.join(MANIFEST_FILE)).unwrap()).unwrap()
}

fn sqlite(p: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open_with_flags(p, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
}

fn count(p: &Path, sql: &str) -> i64 {
    sqlite(p).query_row(sql, [], |r| r.get(0)).unwrap()
}

fn sha_hex(p: &Path) -> String {
    use sha2::Digest;
    let b = std::fs::read(p).unwrap();
    sha2::Sha256::digest(&b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

/// Число жёстких ссылок файла: unix `st_nlink`, Windows `nNumberOfLinks`.
fn nlink(p: &Path) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(p).unwrap().nlink()
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        let f = std::fs::File::open(p).unwrap();
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: дескриптор открытого файла и указатель на локальную структуру.
        unsafe { GetFileInformationByHandle(HANDLE(f.as_raw_handle()), &mut info) }.unwrap();
        u64::from(info.nNumberOfLinks)
    }
}

/// Все файлы каталога (рекурсивно), пути относительно `root` через `/`.
fn tree(root: &Path) -> Vec<String> {
    fn walk(d: &Path, root: &Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, root, out);
            } else {
                let rel = p
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push(rel);
            }
        }
    }
    let mut v = Vec::new();
    if root.exists() {
        walk(root, root, &mut v);
    }
    v.sort();
    v
}

fn zam_server(args: &[&str]) -> std::process::Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_zam-server"));
    c.args(args);
    for (k, _) in std::env::vars().filter(|(k, _)| k.starts_with("ZAM_")) {
        c.env_remove(k);
    }
    c.output().expect("запуск zam-server")
}

fn write_conf(dir: &Path, c: &ServerConfig) -> String {
    let conf = dir.join("zam.toml");
    std::fs::write(&conf, toml::to_string(c).unwrap()).unwrap();
    conf.to_string_lossy().into_owned()
}

// ---------------------------------------------------------------- копия

/// Писатель вставляет и правит заметки во время копии: копия проходит `integrity_check` и
/// `foreign_key_check`, число заметок — между числом до и после копии, ревизии каждой полны.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn backup_while_writing_invariants() {
    let (a, _) = seeded("bk-writing").await;
    let a = Arc::new(a);
    let db = a.st.users.get(uid(&a)).await.unwrap();
    let notes = async |db: &Db| -> i64 {
        db.read(|c| {
            Ok(c.query_row(
                "SELECT count(*) FROM nodes WHERE kind = 'note' AND deleted_at IS NULL",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap()
    };
    let stop_w = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let writer = {
        let (a, stop_w) = (a.clone(), stop_w.clone());
        tokio::spawn(async move {
            let mut k = 0;
            while !stop_w.load(std::sync::atomic::Ordering::SeqCst) {
                let n = a
                    .note(
                        "knowledge",
                        &format!("w/n{k}"),
                        &format!("# N{k}\n\nтекст {k}\n"),
                    )
                    .await;
                let id = n["id"].as_str().unwrap();
                let r = a
                    .tok_h(
                        &a.token,
                        Method::PUT,
                        &format!("/api/v1/nodes/{id}"),
                        Some(json!({"content": format!("# N{k}\n\nправка {k}\n")})),
                        &[("if-match", "1")],
                    )
                    .await;
                assert_eq!(r.status, StatusCode::OK, "{}", r.text());
                k += 1;
            }
            k
        })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    let before = notes(&db).await;
    let rep = run_backup(&cfg_of(&a), BackupOpts::default())
        .await
        .unwrap();
    let after = notes(&db).await;
    stop_w.store(true, std::sync::atomic::Ordering::SeqCst);
    let written = writer.await.unwrap();
    assert!(written > 0);
    let copy = rep.path.join("users").join(format!("{}.db", uid(&a)));
    let c = sqlite(&copy);
    let ic: String = c
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ic, "ok");
    assert!(
        c.prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query([])
            .unwrap()
            .next()
            .unwrap()
            .is_none()
    );
    let n = count(
        &copy,
        "SELECT count(*) FROM nodes WHERE kind = 'note' AND deleted_at IS NULL",
    );
    assert!((before..=after).contains(&n), "{before} ≤ {n} ≤ {after}");
    let broken = count(
        &copy,
        "SELECT count(*) FROM nodes n WHERE kind = 'note' AND current_rev <>
           (SELECT count(*) FROM revisions r WHERE r.node_id = n.id)
           OR current_rev <> (SELECT coalesce(max(rev), 0) FROM revisions r WHERE r.node_id = n.id)",
    );
    assert_eq!(broken, 0, "ревизии неполны");
    drop(c);
    if let Ok(a) = Arc::try_unwrap(a) {
        stop(a).await;
    }
}

/// Параллельная запись во время копии не получает `SQLITE_BUSY` (каждая запись проходит, и
/// быстрее `busy_timeout` писателя); рост `-wal` ≤ записанного × 1,5 + 4 МиБ; после снятия
/// признака WAL снова начинается сначала (кадров не больше `wal_restart_bytes`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn backup_writer_no_busy_and_wal_bound() {
    let (a, _) = seeded("bk-wal").await;
    let db = a.st.users.get(uid(&a)).await.unwrap();
    // база побольше — `VACUUM INTO` держит снимок заметное время
    let fill = "x".repeat(64 * 1024);
    for i in 0..300 {
        let v = fill.clone();
        db.write(move |tx| {
            tx.execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
                rusqlite::params![format!("t18a-fill-{i}"), v],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    let wal = db.path().with_file_name(format!("{}.db-wal", uid(&a)));
    let wal_len = || std::fs::metadata(&wal).map_or(0, |m| m.len());
    let base = wal_len();
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let max_wal = Arc::new(std::sync::atomic::AtomicU64::new(base));
    let writer = {
        let (db, done, max_wal, wal) = (db.clone(), done.clone(), max_wal.clone(), wal.clone());
        tokio::spawn(async move {
            let (mut bytes, mut worst) = (0u64, Duration::ZERO);
            let mut i = 0;
            while !done.load(std::sync::atomic::Ordering::SeqCst) {
                let v = "y".repeat(32 * 1024);
                bytes += v.len() as u64;
                let t = Instant::now();
                db.write(move |tx| {
                    tx.execute(
                        "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
                        rusqlite::params![format!("t18a-w-{i}"), v],
                    )?;
                    Ok(())
                })
                .await
                .expect("запись во время копии");
                worst = worst.max(t.elapsed());
                let l = std::fs::metadata(&wal).map_or(0, |m| m.len());
                max_wal.fetch_max(l, std::sync::atomic::Ordering::SeqCst);
                i += 1;
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            (bytes, worst)
        })
    };
    let rep = run_backup(&cfg_of(&a), BackupOpts::default())
        .await
        .unwrap();
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    let (bytes, worst) = writer.await.unwrap();
    assert!(rep.dbs >= 2);
    assert!(
        worst < zam_store::db::WRITER_BUSY_TIMEOUT,
        "запись ждала {worst:?}"
    );
    let grown = max_wal
        .load(std::sync::atomic::Ordering::SeqCst)
        .saturating_sub(base);
    assert!(
        grown <= bytes * 3 / 2 + 4 * 1024 * 1024,
        "WAL вырос на {grown} при записанных {bytes}"
    );
    // признак снят: фоновый поток снова делает RESTART, WAL начинается сначала
    assert!(!zam_store::backup_flag_path(&a.st.cfg.data_dir).exists());
    let t = Instant::now();
    loop {
        let v = "z".repeat(4096);
        db.write(move |tx| {
            tx.execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES ('t18a-tail', ?1)",
                [v],
            )?;
            Ok(())
        })
        .await
        .unwrap();
        // кадров в WAL — отдельным соединением (читатели пула только для чтения)
        let frames: i64 = rusqlite::Connection::open(db.path())
            .unwrap()
            .query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |r| r.get(1))
            .unwrap();
        if frames * 4096 <= 8 * 1024 * 1024 && !db.backup_active() {
            break;
        }
        assert!(
            t.elapsed() < Duration::from_secs(20),
            "WAL не вернулся к норме: {frames} кадров"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    stop(a).await;
}

/// Брошенный признак копии — файл без удерживающего процесса (`backup` убит: лок снят ОС) или
/// старше `flag_max_hours` — игнорируется с WARN «брошенный признак копии»; удерживаемый и
/// свежий — действует.
#[test]
fn stale_backup_flag_ignored_with_warn() {
    let root = common::tmp("bk-stale-flag");
    let flag = zam_store::backup_flag_path(&root);
    let age = |f: &Path, h: u64| {
        std::fs::File::options()
            .write(true)
            .open(f)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - Duration::from_secs(h * 3600))
            .unwrap();
    };
    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let sink = buf.clone();
    let sub = tracing_subscriber::fmt()
        .with_writer(move || Capture(sink.clone()))
        .with_ansi(false)
        .finish();
    let six = Duration::from_secs(6 * 3600);
    tracing::subscriber::with_default(sub, || {
        assert!(!zam_store::backup_flag_active(&root, six), "признака нет");
        let hold = zam_store::hold_backup_flag(&root).unwrap();
        assert!(
            zam_store::backup_flag_active(&root, six),
            "удерживаемый свежий признак"
        );
        age(&flag, 1);
        assert!(
            zam_store::backup_flag_active(&root, six),
            "удерживается, 1 ч"
        );
        age(&flag, 7);
        assert!(!zam_store::backup_flag_active(&root, six), "старше 6 ч");
        drop(hold);
        // копию убили: файл остался, лок снят ОС
        std::fs::write(&flag, br#"{"pid":1,"started_at":"2026-11-08T00:00:00Z"}"#).unwrap();
        age(&flag, 1);
        assert!(
            !zam_store::backup_flag_active(&root, six),
            "не удерживается"
        );
    });
    let log = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert_eq!(log.matches("брошенный признак копии").count(), 2, "{log}");
}

struct Capture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Закрытые базы без `-wal`/`-shm`, лок занят (сервер «запущен», баз не открывал): копия
/// открывает их `READ_WRITE` + `query_only` и проходит.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn backup_reads_closed_db_without_wal_shm() {
    let (a, _) = seeded("bk-closed").await;
    let cfg = cfg_of(&a);
    let user = uid(&a);
    stop(a).await;
    let root = cfg.data_dir.clone();
    let udb = root.join("users").join(format!("{user}.db"));
    for p in [root.join("system.db"), udb.clone()] {
        for s in ["-wal", "-shm"] {
            let mut x = p.as_os_str().to_owned();
            x.push(s);
            assert!(!PathBuf::from(x).exists(), "{} остался", p.display());
        }
    }
    let lock = DataDir::open(&root).unwrap();
    let rep = run_backup(&cfg, BackupOpts::default()).await.unwrap();
    assert_eq!(rep.dbs, 2);
    assert_eq!(rep.blobs, 2);
    drop(lock);
}

/// Каталог без права создавать файлы (Windows — запрет ACL `WD,AD` для «Все», Linux — 0555).
struct NoCreate(Vec<PathBuf>);

impl NoCreate {
    fn apply(dirs: &[PathBuf]) -> Option<NoCreate> {
        // guard — до первого icacls: паника посреди цикла снимет уже наложенные запреты
        let mut guard = NoCreate(Vec::new());
        for d in dirs {
            guard.0.push(d.clone());
            #[cfg(windows)]
            {
                let st = Command::new("icacls")
                    .arg(d)
                    .args(["/deny", "*S-1-1-0:(WD,AD)"])
                    .output()
                    .unwrap();
                assert!(
                    st.status.success(),
                    "{}",
                    String::from_utf8_lossy(&st.stdout)
                );
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o555)).unwrap();
            }
        }
        // root на Linux права каталога не ограничивают — проверку пропускаем
        let probe = dirs[0].join("probe.tmp");
        if std::fs::write(&probe, b"x").is_ok() {
            let _ = std::fs::remove_file(&probe);
            eprintln!("пропуск: каталог только для чтения не получился (root?)");
            return None;
        }
        Some(guard)
    }
}

impl Drop for NoCreate {
    fn drop(&mut self) {
        for d in &self.0 {
            #[cfg(windows)]
            {
                let _ = Command::new("icacls")
                    .arg(d)
                    .args(["/remove:d", "*S-1-1-0"])
                    .output();
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o755));
            }
        }
    }
}

/// `immutable=1` — только под своим локом: при занятом локе в каталоге только для чтения
/// копия невозможна (код 1), при свободном — `backup` берёт лок сам и читает базы
/// `immutable=1` (без `-shm`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn backup_immutable_only_under_own_lock() {
    let (a, _) = seeded("bk-immutable").await;
    let cfg = cfg_of(&a);
    stop(a).await;
    let root = cfg.data_dir.clone();
    let out_dir = common::tmp("bk-immutable-out");
    let conf = write_conf(&out_dir, &cfg);
    let lock = DataDir::open(&root).unwrap();
    let Some(ro) = NoCreate::apply(&[root.clone(), root.join("users")]) else {
        return;
    };
    let dir_arg = out_dir.join("copies").to_string_lossy().into_owned();
    let out = zam_server(&["backup", "--config", &conf, "--dir", &dir_arg]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("только для чтения"), "{err}");
    let opts = BackupOpts {
        dir: Some(out_dir.join("copies")),
        zip: None,
    };
    assert!(matches!(
        run_backup(&cfg, opts.clone()).await,
        Err(BackupError::ReadOnly { .. })
    ));
    // лок свободен: копия под своим локом, immutable
    drop(lock);
    let rep = run_backup(&cfg, opts).await.unwrap();
    assert_eq!(rep.dbs, 2);
    drop(ro);
    assert!(!root.join("system.db-shm").exists());
}

/// Пользователь, которого нет в снимке `system.db` (его база появилась позже), — в `drift`, а
/// не ошибка; пользователь без базы — пропуск с отметкой.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn backup_order_users_from_system_copy() {
    let (a, _) = seeded("bk-order").await;
    let late = Id::new();
    let users = a.st.cfg.data_dir.join("users");
    std::fs::copy(
        users.join(format!("{}.db", uid(&a))),
        users.join(format!("{late}.db")),
    )
    .unwrap();
    let nofile = a.st.system.create_user("без-базы", false).await.unwrap().id;
    let rep = run_backup(&cfg_of(&a), BackupOpts::default())
        .await
        .unwrap();
    let m = read_manifest(&rep.path);
    assert!(
        rep.drift.iter().any(|d| d.contains(&late.to_string())),
        "{:?}",
        rep.drift
    );
    assert_eq!(m.drift, rep.drift);
    assert!(!m.files.iter().any(|f| f.path.contains(&late.to_string())));
    // база пользователя ещё не создана (воркер работающего сервера открыл бы её сам —
    // проверка на остановленном): пропуск с отметкой
    let cfg = cfg_of(&a);
    stop(a).await;
    let _ = std::fs::remove_file(users.join(format!("{nofile}.db")));
    let rep = run_backup(&cfg, BackupOpts::default()).await.unwrap();
    assert!(
        rep.drift
            .iter()
            .any(|d| d.contains(&nofile.to_string()) && d.contains("базы ещё нет")),
        "{:?}",
        rep.drift
    );
}

/// Мало места (тестовая точка) — отказ до записи: каталог копии не создан.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn backup_checks_free_space() {
    let (a, _) = seeded("bk-space").await;
    let cfg = cfg_of(&a);
    test_hooks::set_free_space(&cfg.data_dir, Some(1000));
    let r = run_backup(&cfg, BackupOpts::default()).await;
    test_hooks::set_free_space(&cfg.data_dir, None);
    match r {
        Err(BackupError::NoSpace { need, free }) => {
            assert_eq!(free, 1000);
            assert!(need > 1000);
        }
        other => panic!("ожидался NoSpace: {other:?}"),
    }
    assert!(tree(&cfg.data_dir.join("backups")).is_empty());
    assert!(!zam_store::backup_flag_path(&cfg.data_dir).exists());
    stop(a).await;
}

/// Во время `restore` (`.zam.lock` с меткой `restore`) копия отказывает.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn backup_refuses_during_restore() {
    let (a, _) = seeded("bk-during-restore").await;
    let cfg = cfg_of(&a);
    stop(a).await;
    let lock = DataDir::open_for_restore(&cfg.data_dir).unwrap();
    assert_eq!(
        zam_store::lock_mark(&cfg.data_dir),
        Some(zam_store::LockMark::Restore)
    );
    assert!(matches!(
        run_backup(&cfg, BackupOpts::default()).await,
        Err(BackupError::RestoreInProgress)
    ));
    drop(lock);
    assert_eq!(zam_store::lock_mark(&cfg.data_dir), None);
    let lock = DataDir::open(&cfg.data_dir).unwrap();
    assert_eq!(
        zam_store::lock_mark(&cfg.data_dir),
        Some(zam_store::LockMark::Serve)
    );
    drop(lock);
}

/// Опись: формат, суммы и размеры всех файлов, версии схем, модели кэша и отпечатки нарезки,
/// мастер-ключ не входит.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn backup_manifest_sums_schemas_chunkers() {
    let (a, shas) = seeded("bk-manifest").await;
    let user = uid(&a);
    let db = a.st.users.get(user).await.unwrap();
    db.write(|tx| {
        tx.execute(
            "INSERT INTO embeddings (text_hash, model_id, dense, sparse, created_at)
             VALUES ('h1', 'm-test@00000000', x'00', x'', '2026-01-01T00:00:00Z')",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let rep = run_backup(&cfg_of(&a), BackupOpts::default())
        .await
        .unwrap();
    let m = read_manifest(&rep.path);
    assert_eq!(m.format, 1);
    assert!(!m.master_key_included);
    assert!(m.created_at.parse::<jiff::Timestamp>().is_ok());
    assert_eq!(m.server_version, env!("CARGO_PKG_VERSION"));
    let urel = format!("users/{user}.db");
    assert_eq!(
        m.schemas.get("system.db"),
        Some(&(zam_store::SYSTEM_MIGRATIONS.len() as u32))
    );
    assert_eq!(m.schemas.get(&urel), Some(&(USER_MIGRATIONS.len() as u32)));
    assert_eq!(
        m.embedding_models[&urel],
        vec!["m-test@00000000".to_string()]
    );
    let ch = &m.chunkers[&urel];
    assert!(
        ch.chunker.is_some() && ch.chunker_entries.is_some(),
        "{ch:?}"
    );
    for f in &m.files {
        let p = rep.path.join(&f.path);
        assert_eq!(std::fs::metadata(&p).unwrap().len(), f.size, "{}", f.path);
        assert_eq!(sha_hex(&p), f.sha256, "{}", f.path);
    }
    for s in &shas {
        assert!(m.files.iter().any(|f| f.path.ends_with(s.as_str())), "{s}");
    }
    assert_eq!(rep.dbs, 2);
    assert_eq!((rep.blobs, rep.copied, rep.linked), (2, 2, 0));
    stop(a).await;
}

/// В копию не входят мастер-ключ, Qdrant, превью, модели и журналы.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn backup_excludes_key_qdrant_previews_models_logs() {
    let (a, _) = seeded("bk-excl").await;
    let root = a.st.cfg.data_dir.clone();
    for d in ["qdrant/storage", "previews/u", "models/m", "logs"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
        std::fs::write(root.join(d).join("x.bin"), b"derived").unwrap();
    }
    assert!(
        root.join("master.key").is_file(),
        "мастер-ключ создан при старте"
    );
    let rep = run_backup(&cfg_of(&a), BackupOpts::default())
        .await
        .unwrap();
    for f in tree(&rep.path) {
        assert!(
            f == MANIFEST_FILE
                || f == "system.db"
                || f.starts_with("users/")
                || f.starts_with("blobs/"),
            "лишний файл в копии: {f}"
        );
    }
    stop(a).await;
}

// ---------------------------------------------------------------- блобы

/// Вторая копия связывает блобы жёсткими ссылками с первой.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn backup_second_run_hardlinks_blobs() {
    let (a, shas) = seeded("bk-links").await;
    let cfg = cfg_of(&a);
    let r1 = run_backup(&cfg, BackupOpts::default()).await.unwrap();
    let r2 = run_backup(&cfg, BackupOpts::default()).await.unwrap();
    assert_ne!(r1.path, r2.path);
    assert_eq!((r2.linked, r2.copied), (2, 0));
    let user = uid(&a);
    let b = r2
        .path
        .join("blobs")
        .join(user.to_string())
        .join(&shas[0][..2])
        .join(&shas[0]);
    assert!(nlink(&b) >= 2, "ссылок {}", nlink(&b));
    stop(a).await;
}

/// Жёсткая ссылка не создаётся (тестовая точка) — блоб копируется: `linked: 0, copied: N`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn backup_blob_fallback_copy() {
    let (a, shas) = seeded("bk-nolink").await;
    let cfg = cfg_of(&a);
    test_hooks::set_link_fails(&cfg.data_dir, true);
    run_backup(&cfg, BackupOpts::default()).await.unwrap();
    let r2 = run_backup(&cfg, BackupOpts::default()).await;
    test_hooks::set_link_fails(&cfg.data_dir, false);
    let r2 = r2.unwrap();
    assert_eq!((r2.linked, r2.copied), (0, shas.len()));
    stop(a).await;
}

/// Файла блоба нет — копия не проходит, отсутствующие перечислены в описи `.failed`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn backup_missing_blob_fails_listed() {
    let (a, shas) = seeded("bk-missing").await;
    let cfg = cfg_of(&a);
    let user = uid(&a);
    let p = cfg
        .data_dir
        .join("blobs")
        .join(user.to_string())
        .join(&shas[1][..2])
        .join(&shas[1]);
    std::fs::remove_file(&p).unwrap();
    match run_backup(&cfg, BackupOpts::default()).await {
        Err(BackupError::MissingBlob { user: u, sha }) => {
            assert_eq!((u, sha), (user.to_string(), shas[1].clone()));
        }
        other => panic!("ожидался MissingBlob: {other:?}"),
    }
    let backups = cfg.data_dir.join("backups");
    let failed: Vec<PathBuf> = std::fs::read_dir(&backups)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().ends_with(".failed"))
        .collect();
    assert_eq!(failed.len(), 1, "{:?}", tree(&backups));
    let m: Value =
        serde_json::from_str(&std::fs::read_to_string(failed[0].join(MANIFEST_FILE)).unwrap())
            .unwrap();
    assert_eq!(m["missing_blobs"], json!([format!("{user}/{}", shas[1])]));
    assert!(zam_server::backup::list_copies(&backups).is_empty());
    stop(a).await;
}

// ---------------------------------------------------------------- отказы restore

/// Готовая копия остановленного сервера и её конфигурация.
async fn copy_of(name: &str) -> (PathBuf, ServerConfig, Id, Vec<String>) {
    let (a, shas) = seeded(name).await;
    let cfg = cfg_of(&a);
    let user = uid(&a);
    let rep = run_backup(&cfg, BackupOpts::default()).await.unwrap();
    stop(a).await;
    (rep.path, cfg, user, shas)
}

fn dst_cfg(name: &str) -> ServerConfig {
    common::cfg_in(common::tmp(name).join("d"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restore_refuses_nonempty() {
    let (copy, src_cfg, _, _) = copy_of("rs-nonempty").await;
    // каталог-источник не пуст
    let r = restore(&src_cfg, &copy, &RestoreOpts::default()).await;
    assert!(matches!(r, Err(BackupError::NotEmpty)), "{r:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restore_replace_moves_old() {
    let (copy, _, user, _) = copy_of("rs-replace").await;
    let dst = dst_cfg("rs-replace-dst");
    let old = api_with(dst.clone()).await;
    let old_admin = uid(&old);
    stop(old).await;
    let opts = RestoreOpts {
        replace: true,
        ..RestoreOpts::default()
    };
    let rep = restore(&dst, &copy, &opts).await.unwrap();
    let moved = rep.moved_to.clone().expect("перенос прежних данных");
    assert!(moved.starts_with(dst.data_dir.join("backups")));
    assert!(moved.join("system.db").is_file());
    assert!(
        moved
            .join("users")
            .join(format!("{old_admin}.db"))
            .is_file()
    );
    assert!(
        dst.data_dir
            .join("users")
            .join(format!("{user}.db"))
            .is_file()
    );
    assert!(
        !dst.data_dir
            .join("users")
            .join(format!("{old_admin}.db"))
            .exists()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restore_refuses_locked() {
    let (copy, _, _, _) = copy_of("rs-locked").await;
    let dst = dst_cfg("rs-locked-dst");
    let lock = DataDir::open(&dst.data_dir).unwrap();
    let r = restore(&dst, &copy, &RestoreOpts::default()).await;
    assert!(matches!(r, Err(BackupError::Locked)), "{r:?}");
    drop(lock);
}

/// Пересчитывает сумму и размер файла `rel` в описи копии `copy`.
fn reseal(copy: &Path, rel: &str) {
    let mut m = read_manifest(copy);
    let p = copy.join(rel);
    let f = m.files.iter_mut().find(|f| f.path == rel).unwrap();
    f.sha256 = sha_hex(&p);
    f.size = std::fs::metadata(&p).unwrap().len();
    std::fs::write(
        copy.join(MANIFEST_FILE),
        serde_json::to_string_pretty(&m).unwrap(),
    )
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restore_schema_too_new() {
    let (copy, _, user, _) = copy_of("rs-too-new").await;
    let rel = format!("users/{user}.db");
    rusqlite::Connection::open(copy.join(&rel))
        .unwrap()
        .pragma_update(None, "user_version", 999)
        .unwrap();
    reseal(&copy, &rel);
    let dst = dst_cfg("rs-too-new-dst");
    match restore(&dst, &copy, &RestoreOpts::default()).await {
        Err(BackupError::SchemaTooNew { file, have, max }) => {
            assert_eq!((file, have, max), (rel, 999, USER_MIGRATIONS.len() as u32));
        }
        other => panic!("ожидался SchemaTooNew: {other:?}"),
    }
    assert!(!dst.data_dir.join("system.db").exists());
}

/// Испорченный блоб копии — отказ до записи хоть одного файла.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restore_bad_sum_before_any_write() {
    let (copy, _, user, shas) = copy_of("rs-badsum").await;
    let rel = format!("blobs/{user}/{}/{}", &shas[1][..2], shas[1]);
    let mut b = std::fs::read(copy.join(&rel)).unwrap();
    b[0] ^= 0xff;
    std::fs::write(copy.join(&rel), b).unwrap();
    let dst = dst_cfg("rs-badsum-dst");
    match restore(&dst, &copy, &RestoreOpts::default()).await {
        Err(BackupError::BadSum { path }) => assert_eq!(path, rel),
        other => panic!("ожидался BadSum: {other:?}"),
    }
    let root = &dst.data_dir;
    assert!(!root.join("system.db").exists());
    assert!(tree(&root.join("users")).is_empty());
    assert!(tree(&root.join("blobs")).is_empty());
}

// ---------------------------------------------------------------- восстановление

/// Копия базы 0024 (M5.7) восстанавливается и при открытии мигрирует до 0025 с `bak-v24`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restore_older_schema_migrates() {
    let src = common::cfg_in(common::tmp("rs-v24").join("d"));
    let user = {
        let dd = DataDir::open(&src.data_dir).unwrap();
        let sys = SystemStore::open(&dd).unwrap();
        let u = sys.create_user("старый", true).await.unwrap().id;
        let db = Db::open(&dd.user_db(&u), &USER_MIGRATIONS[..24]).unwrap();
        drop(db);
        drop(sys);
        u
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    // сервер не запущен: копия под своим локом
    let rep = run_backup(&src, BackupOpts::default()).await.unwrap();
    let rel = format!("users/{user}.db");
    assert_eq!(read_manifest(&rep.path).schemas[&rel], 24);
    let dst = dst_cfg("rs-v24-dst");
    let r = restore(&dst, &rep.path, &RestoreOpts::default())
        .await
        .unwrap();
    assert_eq!(r.schemas[&rel], 24);
    let p = dst.data_dir.join("users").join(format!("{user}.db"));
    assert_eq!(
        count(&p, "PRAGMA user_version"),
        USER_MIGRATIONS.len() as i64
    );
    assert!(p.with_file_name(format!("{user}.db.bak-v24")).is_file());
}

/// Восстановленные блобы — копии, а не жёсткие ссылки на файлы копии.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restore_copies_blobs_not_links() {
    let (copy, _, user, shas) = copy_of("rs-blobs").await;
    let dst = dst_cfg("rs-blobs-dst");
    let r = restore(&dst, &copy, &RestoreOpts::default()).await.unwrap();
    assert_eq!((r.dbs, r.blobs), (2, 2));
    for s in &shas {
        let p = dst
            .data_dir
            .join("blobs")
            .join(user.to_string())
            .join(&s[..2])
            .join(s);
        assert_eq!(sha_hex(&p), *s);
        assert_eq!(nlink(&p), 1, "{}", p.display());
    }
}

/// `web_sessions` после восстановления пусты: cookie старой машины не действуют.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restore_clears_web_sessions() {
    let (copy, _, _, _) = copy_of("rs-web").await;
    assert!(count(&copy.join("system.db"), "SELECT count(*) FROM web_sessions") > 0);
    let dst = dst_cfg("rs-web-dst");
    restore(&dst, &copy, &RestoreOpts::default()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        count(
            &dst.data_dir.join("system.db"),
            "SELECT count(*) FROM web_sessions"
        ),
        0
    );
}

fn reindex_mark(p: &Path) -> Option<String> {
    sqlite(p)
        .query_row(
            "SELECT value FROM meta WHERE key = 'reindex_pending'",
            [],
            |r| r.get(0),
        )
        .ok()
}

/// В каждой восстановленной базе — отметка «пересобрать индекс из кэша», в `qdrant/` —
/// `.zam-reindex`; с `--no-reindex` отметок нет.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restore_sets_reindex_pending_each_db() {
    let (a, _) = seeded("rs-reindex").await;
    let (second, tok) = a.new_user("второй").await;
    let r = a
        .tok(
            &tok,
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": "knowledge", "path": "x", "kind": "note", "content": "# X\n"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let user = uid(&a);
    let rep = run_backup(&cfg_of(&a), BackupOpts::default())
        .await
        .unwrap();
    stop(a).await;
    let dst = dst_cfg("rs-reindex-dst");
    restore(&dst, &rep.path, &RestoreOpts::default())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    for u in [user.to_string(), second.clone()] {
        let p = dst.data_dir.join("users").join(format!("{u}.db"));
        let v: Value = serde_json::from_str(&reindex_mark(&p).expect("отметка")).unwrap();
        assert_eq!(v, json!({"full": false, "space": null}), "{u}");
    }
    assert!(dst.data_dir.join("qdrant").join(".zam-reindex").is_file());

    let dst2 = dst_cfg("rs-reindex-dst2");
    let opts = RestoreOpts {
        reindex: false,
        ..RestoreOpts::default()
    };
    restore(&dst2, &rep.path, &opts).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let p = dst2.data_dir.join("users").join(format!("{user}.db"));
    assert_eq!(reindex_mark(&p), None);
    assert!(!dst2.data_dir.join("qdrant").join(".zam-reindex").exists());
}

/// Ключа Vault нет на новой машине — восстановление проходит, `vault_ok: false`; прежний
/// `master.key` на месте — `true`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restore_without_key_vault_unavailable() {
    let (copy, src_cfg, _, _) = copy_of("rs-vault").await;
    let dst = dst_cfg("rs-vault-dst");
    let r = restore(&dst, &copy, &RestoreOpts::default()).await.unwrap();
    assert!(!r.vault_ok);
    std::fs::copy(
        src_cfg.data_dir.join("master.key"),
        dst.data_dir.join("master.key"),
    )
    .unwrap();
    let opts = RestoreOpts {
        replace: true,
        ..RestoreOpts::default()
    };
    let r = restore(&dst, &copy, &opts).await.unwrap();
    assert!(r.vault_ok);
}

/// Прогноз векторов: другой `model_id` — пересчёт по модели, другой `meta.chunker` —
/// перенарезка; совпали оба — из кэша.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restore_vector_plan_model_and_chunker() {
    let _g = CHUNKER.lock().await;
    let (a, _) = seeded("rs-plan").await;
    let user = uid(&a);
    let db = a.st.users.get(user).await.unwrap();
    db.write(|tx| {
        tx.execute(
            "INSERT INTO embeddings (text_hash, model_id, dense, sparse, created_at)
             VALUES ('h1', 'm1@11111111', x'00', x'', '2026-01-01T00:00:00Z')",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let cfg = cfg_of(&a);
    let same = run_backup(&cfg, BackupOpts::default()).await.unwrap();
    db.write(|tx| {
        tx.execute(
            "UPDATE meta SET value = 'v0;bogus' WHERE key = 'chunker'",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let other_chunker = run_backup(&cfg, BackupOpts::default()).await.unwrap();
    stop(a).await;
    let with_model = |m: &str| RestoreOpts {
        model_id: Some(m.to_string()),
        ..RestoreOpts::default()
    };
    let r = restore(
        &dst_cfg("rs-plan-1"),
        &same.path,
        &with_model("m2@22222222"),
    )
    .await
    .unwrap();
    match r.vectors {
        VectorPlan::Recompute { reason, chunks } => {
            assert_eq!(reason, "model");
            assert!(chunks > 0);
        }
        other => panic!("{other:?}"),
    }
    assert!(r.vectors.text().contains("другая модель") && r.text().contains("другая модель"));
    let r = restore(
        &dst_cfg("rs-plan-2"),
        &same.path,
        &with_model("m1@11111111"),
    )
    .await
    .unwrap();
    assert_eq!(r.vectors, VectorPlan::FromCache);
    let r = restore(
        &dst_cfg("rs-plan-3"),
        &other_chunker.path,
        &with_model("m1@11111111"),
    )
    .await
    .unwrap();
    match r.vectors {
        VectorPlan::Recompute { reason, chunks } => {
            assert_eq!(reason, "chunker");
            assert!(chunks > 0);
        }
        other => panic!("{other:?}"),
    }
}

/// Копия архивом zip восстанавливается так же, как каталог.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn backup_zip_and_restore_from_zip() {
    let (a, shas) = seeded("bk-zip").await;
    let cfg = cfg_of(&a);
    let user = uid(&a);
    let zip = common::tmp("bk-zip-out").join("copy.zip");
    let rep = run_backup(
        &cfg,
        BackupOpts {
            dir: None,
            zip: Some(zip.clone()),
        },
    )
    .await
    .unwrap();
    stop(a).await;
    assert_eq!(rep.path, zip);
    assert!(zip.is_file());
    let dst = dst_cfg("bk-zip-dst");
    let r = restore(&dst, &zip, &RestoreOpts::default()).await.unwrap();
    assert_eq!((r.dbs, r.blobs), (2, 2));
    let p = dst
        .data_dir
        .join("blobs")
        .join(user.to_string())
        .join(&shas[0][..2])
        .join(&shas[0]);
    assert_eq!(sha_hex(&p), shas[0]);
}

// ---------------------------------------------------------------- с моделью и Qdrant

const ML_NOTES: [(&str, &str); 12] = [
    (
        "kb/build",
        "# Сборка\n\nПроект собирается командой cargo build, тесты — cargo test.\n",
    ),
    (
        "kb/deploy",
        "# Развёртывание\n\nСервер ставится службой systemd или Windows.\n",
    ),
    (
        "kb/backup",
        "# Копии\n\nКопия базы снимается командой backup без остановки.\n",
    ),
    (
        "kb/qdrant",
        "# Векторный индекс\n\nQdrant хранит точки, пересобирается из кэша.\n",
    ),
    (
        "kb/vault",
        "# Секреты\n\nVault шифрует значения мастер-ключом сервера.\n",
    ),
    (
        "kb/tokens",
        "# Токены\n\nАгент получает токен доступа и адрес сервера.\n",
    ),
    (
        "kb/search",
        "# Поиск\n\nГибридный поиск: полнотекстовый и векторный каналы.\n",
    ),
    (
        "kb/chunks",
        "# Нарезка\n\nЗаметка делится на чанки по заголовкам и абзацам.\n",
    ),
    (
        "kb/hooks",
        "# Хуки\n\nХук сессии вызывает recall в начале работы агента.\n",
    ),
    (
        "kb/cache",
        "# Кэш\n\nВыжимки агентов хранятся в кэше с оценкой свежести.\n",
    ),
    (
        "kb/linux",
        "# Linux\n\nСтенд Debian с двумя ядрами CPU и двумя гигабайтами памяти.\n",
    ),
    (
        "kb/gpu",
        "# GPU\n\nМодель fp16 работает на CUDA, int8 — на процессоре.\n",
    ),
];

const ML_QUERIES: [&str; 10] = [
    "как собрать проект",
    "служба systemd",
    "копия без остановки",
    "пересборка индекса",
    "шифрование секретов",
    "токен агента",
    "гибридный поиск",
    "чанки заметки",
    "recall в начале сессии",
    "стенд Debian",
];

/// Выдача на [`ML_QUERIES`] токеном `token` (токены переезжают с копией `system.db`) в новой
/// сессии `session` (выданное сессии понижается, а сессии тоже переезжают): по запросу —
/// `(путь, балл)` первых шести (шестой — только чтобы увидеть ничью на пятом месте).
async fn top6(a: &Api, token: &str, session: &str) -> Vec<Vec<(String, f64)>> {
    let mut out = Vec::new();
    for q in ML_QUERIES {
        let r = a
            .tok_h(
                token,
                Method::POST,
                "/api/v1/search",
                Some(json!({"query": q, "top_k": 6})),
                &[("x-zam-session", session)],
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        let j = r.json();
        out.push(
            common::attach::items(&j)
                .iter()
                .map(|i| {
                    (
                        i["path"].as_str().unwrap_or_default().to_string(),
                        i["score"].as_f64().unwrap_or_default(),
                    )
                })
                .collect(),
        );
    }
    out
}

/// Топ-5 совпадают: баллы первых пяти равны, пути — на каждом месте, где балл не делит ничью
/// с соседом (порядок равных баллов от Qdrant не зависит от содержимого).
fn same_top5(before: &[Vec<(String, f64)>], after: &[Vec<(String, f64)>]) {
    const EPS: f64 = 1e-4;
    for (k, (x, y)) in before.iter().zip(after).enumerate() {
        let n = x.len().min(5);
        assert_eq!(n, y.len().min(5), "запрос {k}: {x:?} / {y:?}");
        for i in 0..n {
            assert!(
                (x[i].1 - y[i].1).abs() < EPS,
                "запрос {k}, место {i}: {x:?} / {y:?}"
            );
            let tied = x
                .iter()
                .enumerate()
                .any(|(j, (_, s))| j != i && (s - x[i].1).abs() < EPS);
            if !tied {
                assert_eq!(x[i].0, y[i].0, "запрос {k}, место {i}: {x:?} / {y:?}");
            }
        }
    }
}

async fn index_all(a: &Api) {
    for (p, t) in ML_NOTES {
        a.note("knowledge", p, t).await;
    }
    let db = a.st.users.get(uid(a)).await.unwrap();
    common::ml::until(
        "все заметки проиндексированы",
        common::ml::READY,
        async || {
            db.read(|c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM nodes WHERE kind = 'note' AND index_state <> 'indexed'",
                    [],
                    |r| r.get::<_, i64>(0),
                )?)
            })
            .await
            .unwrap()
                == 0
        },
    )
    .await;
}

async fn points(a: &Api) -> u64 {
    let f = common::api::vector_base(a, uid(a)).await;
    a.st.index.vector.as_ref().unwrap().count(&f).await.unwrap()
}

/// Точки после затихания воркера (пачки и свёртки L0 папок закончены, число не меняется).
async fn settled_points(a: &Api) -> u64 {
    let mut last = u64::MAX;
    let t = Instant::now();
    loop {
        let w = a.st.index.worker.status();
        let n = points(a).await;
        if !w.batch_running && w.rollups_pending == 0 && !w.reindex_running && n == last {
            return n;
        }
        assert!(t.elapsed() < common::ml::READY, "воркер не затих: {w:?}");
        last = n;
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Внешний Qdrant (без супервизора, флаг `.zam-reindex` не читается): первая же пересборка по
/// `meta.reindex_pending` пишет точки из кэша.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restore_external_qdrant_rebuilds_from_cache() {
    use common::ml::{MINILM, external, have, ml};
    if !have(MINILM) {
        return;
    }
    let _g = CHUNKER.lock().await;
    let qa = common::qdrant::TestQdrant::start("rs-ext-a");
    let a = api("rs-ext-a", |c| {
        ml(c, MINILM);
        external(c, &qa);
    })
    .await;
    common::ml::vector_ready(&a).await;
    index_all(&a).await;
    let want = settled_points(&a).await;
    assert!(want > 0);
    let rep = run_backup(&cfg_of(&a), BackupOpts::default())
        .await
        .unwrap();
    stop(a).await;
    drop(qa);

    let qb = common::qdrant::TestQdrant::start("rs-ext-b");
    let mut dst = dst_cfg("rs-ext-b");
    ml(&mut dst, MINILM);
    external(&mut dst, &qb);
    let r = restore(&dst, &rep.path, &RestoreOpts::default())
        .await
        .unwrap();
    assert_eq!(r.vectors, VectorPlan::FromCache, "{}", r.text());
    let b = api_with(dst.clone()).await;
    common::ml::until_diag(
        "точки из кэша",
        common::ml::READY,
        async || b.st.index.vector_available().is_ok() && points(&b).await == want,
        async || format!("ожидалось {want}; {}", common::ml::vector_diag(&b)),
    )
    .await;
    let db = b.st.users.get(uid(&b)).await.unwrap();
    common::ml::until(
        "отметка reindex снята",
        common::ml::READY,
        async || zam_store::pending_reindex(&db).await.unwrap().is_none(),
    )
    .await;
    assert_eq!(b.st.index.worker.embedded_texts(), 0, "без инференса");
    stop(b).await;
}

/// Совпали `model_id` и `meta.chunker`: первый `serve` (управляемый Qdrant) пересобирает
/// индекс из кэша — модель не вызывается, перенарезки нет, топ-5 на 10 запросах совпадает.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restore_then_serve_rebuilds_from_cache_no_inference() {
    use common::ml::{MINILM, external, have, ml};
    use common::qdrant::{free_port, qdrant_bin};
    if !have(MINILM) {
        return;
    }
    let _g = CHUNKER.lock().await;
    let qa = common::qdrant::TestQdrant::start("rs-serve-a");
    let a = api("rs-serve-a", |c| {
        ml(c, MINILM);
        external(c, &qa);
    })
    .await;
    common::ml::vector_ready(&a).await;
    index_all(&a).await;
    let want = settled_points(&a).await;
    let token = a.token.clone();
    let before = top6(&a, &token, "t18a-before").await;
    let rep = run_backup(&cfg_of(&a), BackupOpts::default())
        .await
        .unwrap();
    stop(a).await;
    drop(qa);

    let mut dst = dst_cfg("rs-serve-b");
    ml(&mut dst, MINILM);
    dst.qdrant.mode = zam_server::config::QdrantMode::Managed;
    dst.qdrant.binary = Some(qdrant_bin());
    dst.qdrant.http_port = free_port();
    dst.qdrant.grpc_port = free_port();
    let r = restore(&dst, &rep.path, &RestoreOpts::default())
        .await
        .unwrap();
    assert_eq!(r.vectors, VectorPlan::FromCache, "{}", r.text());
    let restored = dst
        .data_dir
        .join("users")
        .join(format!("{}.db", r_user(&rep.path)));
    let seq_before = count(&restored, "SELECT coalesce(max(seq), 0) FROM chunks");
    let b = api_with(dst.clone()).await;
    common::ml::until_diag(
        "точки из кэша",
        common::ml::READY,
        async || b.st.index.vector_available().is_ok() && points(&b).await == want,
        async || {
            format!(
                "ожидалось {want}, есть {}; {}",
                points(&b).await,
                common::ml::vector_diag(&b)
            )
        },
    )
    .await;
    let db = b.st.users.get(uid(&b)).await.unwrap();
    common::ml::until(
        "отметка reindex снята",
        common::ml::READY,
        async || zam_store::pending_reindex(&db).await.unwrap().is_none(),
    )
    .await;
    // стёртое хранилище и новая коллекция не перенарезают базу с отметкой reindex: ни
    // новых чанков, ни заданий
    assert_eq!(settled_points(&b).await, want);
    let (seq, jobs) = db
        .read(|c| {
            Ok((
                c.query_row("SELECT coalesce(max(seq), 0) FROM chunks", [], |r| {
                    r.get::<_, i64>(0)
                })?,
                c.query_row(
                    "SELECT count(*) FROM nodes WHERE kind = 'note' AND index_state <> 'indexed'",
                    [],
                    |r| r.get::<_, i64>(0),
                )?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(seq, seq_before, "чанки не перенарезаны");
    assert_eq!(jobs, 0, "заметки не ставились в очередь заново");
    assert_eq!(
        b.st.index.worker.embedded_texts(),
        0,
        "модель не вызывалась"
    );
    assert!(
        b.st.users.rechunk_progress(uid(&b)).await.is_none(),
        "перенарезки нет"
    );
    same_top5(&before, &top6(&b, &token, "t18a-after").await);
    stop(b).await;
}

/// Пользователь единственной базы копии.
fn r_user(copy: &Path) -> String {
    let m = read_manifest(copy);
    m.files
        .iter()
        .find_map(|f| {
            f.path
                .strip_prefix("users/")
                .and_then(|x| x.strip_suffix(".db"))
                .map(str::to_string)
        })
        .unwrap()
}

// ---------------------------------------------------------------- расписание

fn at(s: &str) -> jiff::Timestamp {
    s.parse().unwrap()
}

#[test]
fn schedule_tick_cases() {
    let daily = BackupSection {
        schedule: BackupSchedule::Daily,
        ..BackupSection::default()
    };
    let off = BackupSection::default();
    let copies = |days: &[u32]| -> Vec<PathBuf> {
        days.iter()
            .map(|d| PathBuf::from(format!("b/2026-11-{d:02}_033000")))
            .collect()
    };
    // выключено — всегда пропуск
    assert_eq!(
        tick(at("2026-11-08T04:00:00Z"), None, &[], &off),
        vec![Action::Skip]
    );
    // до at — пропуск, после — копия
    assert_eq!(
        tick(
            at("2026-11-08T03:29:59Z"),
            Some(at("2026-11-07T03:30:00Z")),
            &copies(&[7]),
            &daily
        ),
        vec![Action::Skip]
    );
    assert_eq!(
        tick(
            at("2026-11-08T03:30:00Z"),
            Some(at("2026-11-07T03:30:00Z")),
            &copies(&[7]),
            &daily
        ),
        vec![Action::Backup]
    );
    // первая копия — только с сегодняшнего at
    assert_eq!(
        tick(at("2026-11-08T01:00:00Z"), None, &[], &daily),
        vec![Action::Skip]
    );
    assert_eq!(
        tick(at("2026-11-08T05:00:00Z"), None, &[], &daily),
        vec![Action::Backup]
    );
    // сегодня копия уже была
    assert_eq!(
        tick(
            at("2026-11-08T23:00:00Z"),
            Some(at("2026-11-08T03:30:05Z")),
            &copies(&[8]),
            &daily
        ),
        vec![Action::Skip]
    );
    // 9 суток копий при keep = 7 — удаление двух старших
    let nine = copies(&[1, 2, 3, 4, 5, 6, 7, 8, 9]);
    assert_eq!(
        tick(
            at("2026-11-09T05:00:00Z"),
            Some(at("2026-11-09T03:30:00Z")),
            &nine,
            &daily
        ),
        vec![Action::Prune(nine[..2].to_vec())]
    );
    // срок копии — только копия: лишние удаляются следующим шагом после успешной копии
    let eight = copies(&[1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(
        tick(
            at("2026-11-09T03:31:00Z"),
            Some(at("2026-11-08T03:30:00Z")),
            &eight,
            &daily
        ),
        vec![Action::Backup]
    );
}

/// `GET /index/status` → `backup`: копии нет — `no_recent_backup`; после копии — время и без
/// предупреждения.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn index_status_no_recent_backup() {
    let a = api("bk-status", |_| {}).await;
    let s = a.t(Method::GET, "/api/v1/index/status", None).await;
    assert_eq!(s.status, StatusCode::OK, "{}", s.text());
    assert_eq!(
        s.json()["backup"],
        json!({"last_at": null, "schedule": "off", "warning": "no_recent_backup"})
    );
    run_backup(&cfg_of(&a), BackupOpts::default())
        .await
        .unwrap();
    let b = a.t(Method::GET, "/api/v1/index/status", None).await.json()["backup"].clone();
    assert!(b["last_at"].is_string(), "{b}");
    assert_eq!(b["warning"], Value::Null);
    // старая копия (больше warn_days) — снова предупреждение
    let cfg = cfg_of(&a);
    let later = zam_store::time::now() + jiff::SignedDuration::from_hours(24 * 8);
    assert_eq!(
        zam_server::backup::status_json(&cfg, later)["warning"],
        "no_recent_backup"
    );
    stop(a).await;
}

// ---------------------------------------------------------------- правки ревью 18a

/// Копирует дерево каталога (без лока) — «снимок диска» в момент аварии.
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let p = e.path();
        let name = e.file_name();
        if name == ".zam.lock" {
            continue;
        }
        if p.is_dir() {
            copy_tree(&p, &to.join(&name));
        } else {
            std::fs::copy(&p, to.join(&name)).unwrap();
        }
    }
}

/// C1: сервер убит — у базы `-wal` с закоммиченными кадрами, лок свободен. Копия под своим
/// локом читает базу с восстановлением WAL (не `immutable=1`): в копии все записи.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn backup_crashed_wal_under_own_lock_keeps_commits() {
    let (a, _) = seeded("bk-crash-wal").await;
    let cfg = cfg_of(&a);
    let user = uid(&a);
    stop(a).await;
    let udb = cfg.data_dir.join("users").join(format!("{user}.db"));
    // писатель «падает» с кадрами в WAL: контрольных точек нет, соединение живо, пока
    // делается снимок диска
    let c = rusqlite::Connection::open(&udb).unwrap();
    c.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    let mode: String = c
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode.to_lowercase(), "wal");
    for i in 0..100 {
        c.execute(
            "INSERT INTO meta (key, value) VALUES (?1, 'x')",
            [format!("t18a-crash-{i}")],
        )
        .unwrap();
    }
    let crashed = common::tmp("bk-crash-wal-disk").join("d");
    copy_tree(&cfg.data_dir, &crashed);
    drop(c);
    let cudb = crashed.join("users").join(format!("{user}.db"));
    assert!(
        zam_store::backup::wal_size(&cudb) > 0,
        "в снимке диска нет WAL"
    );
    let mut c2 = cfg.clone();
    c2.data_dir = crashed.clone();
    let rep = run_backup(&c2, BackupOpts::default()).await.unwrap();
    let copy = rep.path.join("users").join(format!("{user}.db"));
    assert_eq!(
        count(
            &copy,
            "SELECT count(*) FROM meta WHERE key LIKE 't18a-crash-%'"
        ),
        100
    );
}

/// I2: секрет Vault первого пользователя, созданный во время копии (ключ `user_keys` — после
/// первого снимка `system.db`): в копию идёт второй снимок `system.db`, ключ в нём есть.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn backup_system_snapshot_after_user_dbs() {
    let (a, _) = seeded("bk-sys-second").await;
    let cfg = cfg_of(&a);
    let user = uid(&a);
    let sys = cfg.data_dir.join("system.db");
    let uid_s = user.to_string();
    test_hooks::set_after_first_snapshot(
        &cfg.data_dir,
        Some(Arc::new(move || {
            let c = rusqlite::Connection::open(&sys).unwrap();
            c.busy_timeout(Duration::from_secs(10)).unwrap();
            c.execute(
                "INSERT OR IGNORE INTO user_keys (user_id, wrapped, created_at)
                 VALUES (?1, x'00', '2026-11-08T00:00:00Z')",
                [&uid_s],
            )
            .unwrap();
        })),
    );
    let r = run_backup(&cfg, BackupOpts::default()).await;
    test_hooks::set_after_first_snapshot(&cfg.data_dir, None);
    let rep = r.unwrap();
    assert_eq!(
        count(
            &rep.path.join("system.db"),
            &format!("SELECT count(*) FROM user_keys WHERE user_id = '{user}'")
        ),
        1,
        "ключ Vault, записанный во время копии, в копии system.db"
    );
    stop(a).await;
}

/// I5: архив — место с запасом на сборку и zip; сбой записи архива не оставляет `.tmp` и
/// сборки.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn backup_zip_space_and_tmp_cleanup() {
    let (a, _) = seeded("bk-zip-fail").await;
    let cfg = cfg_of(&a);
    let out = common::tmp("bk-zip-fail-out");
    let zip = out.join("copy.zip");
    let opts = BackupOpts {
        dir: None,
        zip: Some(zip.clone()),
    };
    // места хватает каталогу, но не сборке и архиву вместе
    test_hooks::set_free_space(&cfg.data_dir, Some(1));
    let r = run_backup(&cfg, BackupOpts::default()).await;
    test_hooks::set_free_space(&cfg.data_dir, None);
    let dir_need = match r {
        Err(BackupError::NoSpace { need, .. }) => need,
        other => panic!("{other:?}"),
    };
    test_hooks::set_free_space(&cfg.data_dir, Some(dir_need + dir_need / 2));
    let r = run_backup(&cfg, opts.clone()).await;
    test_hooks::set_free_space(&cfg.data_dir, None);
    assert!(matches!(r, Err(BackupError::NoSpace { .. })), "{r:?}");
    test_hooks::set_fail_point(&cfg.data_dir, Some("zip"));
    let r = run_backup(&cfg, opts.clone()).await;
    test_hooks::set_fail_point(&cfg.data_dir, None);
    assert!(r.is_err());
    assert_eq!(tree(&out), Vec::<String>::new(), "остатки архива");
    run_backup(&cfg, opts).await.unwrap();
    assert_eq!(tree(&out), vec!["copy.zip".to_string()]);
    stop(a).await;
}

/// I6: остаток `system.db-wal` без `system.db` — каталог не пуст; с `--replace` он уходит в
/// `pre-restore-…`, а не применяется к восстановленной базе.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restore_leftover_wal_is_nonempty() {
    let (copy, _, _, _) = copy_of("rs-leftover").await;
    let dst = dst_cfg("rs-leftover-dst");
    std::fs::create_dir_all(&dst.data_dir).unwrap();
    std::fs::write(dst.data_dir.join("system.db-wal"), b"stale wal").unwrap();
    let r = restore(&dst, &copy, &RestoreOpts::default()).await;
    assert!(matches!(r, Err(BackupError::NotEmpty)), "{r:?}");
    let opts = RestoreOpts {
        replace: true,
        ..RestoreOpts::default()
    };
    let rep = restore(&dst, &copy, &opts).await.unwrap();
    let moved = rep.moved_to.unwrap();
    assert!(moved.join("system.db-wal").is_file());
    assert!(
        !dst.data_dir.join("system.db-wal").exists() || {
            // открытие восстановленной базы создаёт свой WAL — не прежний
            std::fs::read(dst.data_dir.join("system.db-wal")).unwrap() != b"stale wal"
        }
    );
}

/// I1: сбой посередине переноса на место (тестовая точка) — каталог помечен незавершённым:
/// сервер и копия отказывают; повтор `restore --replace` восстанавливает.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restore_crash_mid_place_refused_until_replace() {
    let (copy, _, user, _) = copy_of("rs-crash").await;
    let dst = dst_cfg("rs-crash-dst");
    test_hooks::set_fail_point(&dst.data_dir, Some("restore_place"));
    let r = restore(&dst, &copy, &RestoreOpts::default()).await;
    test_hooks::set_fail_point(&dst.data_dir, None);
    match &r {
        Err(e @ BackupError::RestoreFailed { .. }) => {
            assert!(e.to_string().contains("--replace"), "{e}");
        }
        other => panic!("ожидался RestoreFailed: {other:?}"),
    }
    assert!(zam_store::restore_incomplete(&dst.data_dir));
    // сервер не принимает полувосстановленный каталог
    let Err(e) = zam_server::build(dst.clone(), Some("pw".into())).await else {
        panic!("сервер принял полувосстановленный каталог");
    };
    assert!(e.to_string().contains("не завершено"), "{e}");
    assert!(matches!(
        run_backup(&dst, BackupOpts::default()).await,
        Err(BackupError::Store(
            zam_store::StoreError::RestoreIncomplete(_)
        ))
    ));
    // без --replace — «не пуст»
    assert!(matches!(
        restore(&dst, &copy, &RestoreOpts::default()).await,
        Err(BackupError::NotEmpty)
    ));
    let opts = RestoreOpts {
        replace: true,
        ..RestoreOpts::default()
    };
    let rep = restore(&dst, &copy, &opts).await.unwrap();
    assert!(rep.moved_to.is_some());
    assert!(!zam_store::restore_incomplete(&dst.data_dir));
    assert!(
        dst.data_dir
            .join("users")
            .join(format!("{user}.db"))
            .is_file()
    );
    let b = api_with(dst).await;
    assert_eq!(uid(&b), user, "прежний администратор, не новый");
    stop(b).await;
}

/// M7: во время копии (признак `.zam-backup` удерживается) restore отказывает.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restore_refuses_during_backup() {
    let (copy, _, _, _) = copy_of("rs-during-backup").await;
    let dst = dst_cfg("rs-during-backup-dst");
    std::fs::create_dir_all(&dst.data_dir).unwrap();
    let hold = zam_store::hold_backup_flag(&dst.data_dir).unwrap();
    let r = restore(&dst, &copy, &RestoreOpts::default()).await;
    assert!(matches!(r, Err(BackupError::BackupInProgress)), "{r:?}");
    drop(hold);
    restore(&dst, &copy, &RestoreOpts::default()).await.unwrap();
}

/// I3, I4: неудачная автокопия не удаляет хорошие копии (`keep = 1`) и не повторяется каждую
/// минуту — следующая попытка в следующий срок; удачная — ротация по новому списку.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn backup_tick_failure_keeps_copies_and_waits_slot() {
    use zam_server::maintenance::{BackupState, backup_tick};
    let (a, _) = seeded("bk-tick").await;
    let mut cfg = cfg_of(&a);
    cfg.backup.schedule = BackupSchedule::Daily;
    cfg.backup.keep = 1;
    let dir = cfg.backup.dir(&cfg.data_dir);
    let good = run_backup(&cfg, BackupOpts::default()).await.unwrap().path;
    // копия «вчерашняя»: срок сегодняшнего at наступил
    let yesterday = dir.join("2000-01-01_033000");
    std::fs::rename(&good, &yesterday).unwrap();
    let now = at("2026-11-08T04:00:00Z");
    let mut st = BackupState::default();
    test_hooks::set_free_space(&cfg.data_dir, Some(1));
    let o = backup_tick(&cfg, now, &mut st).await;
    assert_eq!((o.backup, o.pruned), (Some(false), 0));
    assert!(
        yesterday.join(MANIFEST_FILE).is_file(),
        "хорошая копия цела"
    );
    // минута спустя — без новой попытки
    let o = backup_tick(&cfg, now + jiff::SignedDuration::from_mins(1), &mut st).await;
    assert_eq!((o.backup, o.pruned), (None, 0));
    assert_eq!(st.failures, 1);
    test_hooks::set_free_space(&cfg.data_dir, None);
    // следующий срок — копия и ротация по фактическому списку
    let o = backup_tick(&cfg, now + jiff::SignedDuration::from_hours(24), &mut st).await;
    assert_eq!((o.backup, o.pruned), (Some(true), 1));
    let left = zam_server::backup::list_copies(&dir);
    assert_eq!(left.len(), 1);
    assert_ne!(left[0], yesterday);
    stop(a).await;
}

/// M11: на Linux копия доступна только владельцу.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn backup_owner_only_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let (a, _) = seeded("bk-perms").await;
    let rep = run_backup(&cfg_of(&a), BackupOpts::default())
        .await
        .unwrap();
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&rep.path), 0o700);
    assert_eq!(mode(&rep.path.join("system.db")), 0o600);
    assert_eq!(mode(&rep.path.join(MANIFEST_FILE)), 0o600);
    stop(a).await;
}

// ---------------------------------------------------------------- повторное ревью 18a

/// Делает файл «старым» (время изменения — `h` часов назад).
fn age_file(p: &Path, h: u64) {
    std::fs::File::options()
        .write(true)
        .open(p)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - Duration::from_secs(h * 3600))
        .unwrap();
}

/// N2: удаляются только свои брошенные сборки — `yyyy-MM-dd_HHmmss.partial` каталога копий и
/// `<архив>.partial`/`<архив>.tmp` (любое расширение архива); чужие `*.partial`/`*.tmp` остаются.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn backup_cleans_only_own_partials() {
    let (a, _) = seeded("bk-own-partials").await;
    let mut cfg = cfg_of(&a);
    cfg.backup.flag_max_hours = 1;
    let copies = cfg.backup.dir(&cfg.data_dir);
    let out = common::tmp("bk-own-partials-out");
    std::fs::create_dir_all(&copies).unwrap();
    let mine = [
        copies.join("2026-01-01_000000.partial"),
        out.join("copy.bin.tmp"),
        out.join("copy.bin.partial"),
    ];
    let foreign = [
        copies.join("notes.partial"),
        copies.join("x.zip.tmp"),
        out.join("other.zip.tmp"),
        out.join("my.partial"),
        out.join("copy.tmp"),
    ];
    for p in mine.iter().chain(&foreign) {
        std::fs::write(p, b"x").unwrap();
        age_file(p, 2);
    }
    run_backup(&cfg, BackupOpts::default()).await.unwrap();
    run_backup(
        &cfg,
        BackupOpts {
            dir: None,
            zip: Some(out.join("copy.bin")),
        },
    )
    .await
    .unwrap();
    for p in &mine {
        assert!(!p.exists(), "своя сборка осталась: {}", p.display());
    }
    for p in &foreign {
        assert!(p.exists(), "чужой файл удалён: {}", p.display());
    }
    stop(a).await;
}

/// N3: повторные сбои восстановления складывают прежние данные в один `pre-restore-…`
/// (из признака незавершённого восстановления), остатки прерванного переноса — в его
/// `restore-remnants-…`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restore_repeated_failures_single_pre_restore() {
    let (copy, _, user, _) = copy_of("rs-repeat").await;
    let dst = dst_cfg("rs-repeat-dst");
    let old = api_with(dst.clone()).await;
    let old_admin = uid(&old);
    stop(old).await;
    let opts = RestoreOpts {
        replace: true,
        ..RestoreOpts::default()
    };
    let mut first = None;
    for _ in 0..2 {
        test_hooks::set_fail_point(&dst.data_dir, Some("restore_place"));
        let r = restore(&dst, &copy, &opts).await;
        test_hooks::set_fail_point(&dst.data_dir, None);
        match r {
            Err(BackupError::RestoreFailed {
                moved_to: Some(m), ..
            }) => {
                assert_eq!(
                    first.get_or_insert(m.clone()),
                    &m,
                    "один каталог прежних данных"
                );
            }
            other => panic!("ожидался RestoreFailed с moved_to: {other:?}"),
        }
    }
    let rep = restore(&dst, &copy, &opts).await.unwrap();
    let pre = first.unwrap();
    assert_eq!(rep.moved_to.as_ref(), Some(&pre));
    let pres: Vec<_> = std::fs::read_dir(dst.data_dir.join("backups"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("pre-restore-"))
        .collect();
    assert_eq!(pres.len(), 1, "{:?}", tree(&dst.data_dir.join("backups")));
    // прежние данные целы и на месте в одном каталоге
    assert!(pre.join("users").join(format!("{old_admin}.db")).is_file());
    let sys = pre.join("system.db");
    assert_eq!(
        count(
            &sys,
            &format!("SELECT count(*) FROM users WHERE id = '{old_admin}'")
        ),
        1
    );
    assert!(
        dst.data_dir
            .join("users")
            .join(format!("{user}.db"))
            .is_file()
    );
}

/// P4: сборку, лок которой держит живой процесс копии, чистильщик не трогает даже старой; с
/// освобождённым локом (копия умерла) — удаляет сразу.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn backup_keeps_live_stage_removes_dead() {
    let (a, _) = seeded("bk-live-stage").await;
    let cfg = cfg_of(&a);
    let copies = cfg.backup.dir(&cfg.data_dir);
    let live = copies.join("2026-01-02_000000.partial");
    let dead = copies.join("2026-01-03_000000.partial");
    for d in [&live, &dead] {
        std::fs::create_dir_all(d).unwrap();
        std::fs::write(d.join(".zam-building"), b"").unwrap();
    }
    let lock = std::fs::File::options()
        .read(true)
        .write(true)
        .open(live.join(".zam-building"))
        .unwrap();
    fs4::FileExt::lock(&lock).unwrap();
    run_backup(&cfg, BackupOpts::default()).await.unwrap();
    assert!(live.is_dir(), "живая сборка удалена");
    assert!(!dead.exists(), "брошенная сборка осталась");
    fs4::FileExt::unlock(&lock).unwrap();
    drop(lock);
    run_backup(&cfg, BackupOpts::default()).await.unwrap();
    assert!(!live.exists(), "сборка с освобождённым локом осталась");
    stop(a).await;
}
