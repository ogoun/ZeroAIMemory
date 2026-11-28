//! `zam-server rollback --to v<N> [--system v<M>] [--dry-run] [--yes] [--json]` (M7.4 §7.2).
//!
//! Каталог данных остановленного сервера собирается напрямую (`DataDir`, `SystemStore`, `Db`):
//! базы пользователей обновлены с v25 до последней схемы — мигратор оставил копию `bak-v25`;
//! метка `meta.probe` различает копию (`v25`) и текущую базу (`v27`).

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use zam_core::Id;
use zam_server::backup::rollback::{RollbackOpts, parse_version, rollback};
use zam_server::backup::{BackupError, BackupOpts, backup, test_hooks};
use zam_server::config::ServerConfig;
use zam_store::{DataDir, Db, SYSTEM_MIGRATIONS, StoreError, SystemStore, USER_MIGRATIONS};

const LATEST: u32 = USER_MIGRATIONS.len() as u32;

struct Fx {
    cfg: ServerConfig,
    root: PathBuf,
    /// Базы обновлены с v25 (копия `bak-v25`).
    upgraded: Vec<Id>,
    /// Базы на v25 (не обновлялись).
    at25: Vec<Id>,
}

impl Fx {
    fn db(&self, u: &Id) -> PathBuf {
        self.root.join("users").join(format!("{u}.db"))
    }
}

fn set_probe(p: &Path, v: &str) {
    Connection::open(p)
        .unwrap()
        .execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('probe', ?1)",
            [v],
        )
        .unwrap();
}

fn ro(p: &Path) -> Connection {
    Connection::open_with_flags(p, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
}

fn probe(p: &Path) -> String {
    ro(p)
        .query_row("SELECT value FROM meta WHERE key = 'probe'", [], |r| {
            r.get(0)
        })
        .unwrap()
}

fn version(p: &Path) -> u32 {
    ro(p)
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap()
}

fn sys_probe(p: &Path) -> String {
    ro(p)
        .query_row("SELECT value FROM settings WHERE key = 'probe'", [], |r| {
            r.get(0)
        })
        .unwrap()
}

fn set_sys_probe(p: &Path, v: &str) {
    Connection::open(p)
        .unwrap()
        .execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES ('probe', ?1)",
            [format!("\"{v}\"")],
        )
        .unwrap();
}

/// Каталог данных: `upgraded` баз, обновлённых с v25, и `at25` баз на v25; с `sys_v6` —
/// `system.db`, обновлённая с v6 (копия `system.db.bak-v6`).
async fn fixture(name: &str, upgraded: usize, at25: usize, sys_v6: bool) -> Fx {
    let cfg = common::cfg_in(common::tmp(name).join("d"));
    let root = cfg.data_dir.clone();
    let dd = DataDir::open(&root).unwrap();
    if sys_v6 {
        drop(Db::open(&dd.system_db(), &SYSTEM_MIGRATIONS[..6]).unwrap());
        set_sys_probe(&dd.system_db(), "v6");
    }
    let sys = SystemStore::open(&dd).unwrap();
    let mut fx = Fx {
        cfg: cfg.clone(),
        root: root.clone(),
        upgraded: Vec::new(),
        at25: Vec::new(),
    };
    for i in 0..upgraded + at25 {
        let u = sys.create_user(&format!("u{i}"), false).await.unwrap().id;
        let p = dd.user_db(&u);
        drop(Db::open(&p, &USER_MIGRATIONS[..25]).unwrap());
        set_probe(&p, "v25");
        if i < upgraded {
            drop(Db::open(&p, USER_MIGRATIONS).unwrap());
            set_probe(&p, "v27");
            fx.upgraded.push(u);
        } else {
            fx.at25.push(u);
        }
    }
    drop(sys);
    if sys_v6 {
        set_sys_probe(&dd.system_db(), "v7");
    }
    drop(dd);
    fx
}

fn opts(to: u32, system: Option<u32>, dry_run: bool, yes: bool) -> RollbackOpts {
    RollbackOpts {
        to,
        system,
        dry_run,
        yes,
    }
}

fn sha_hex(p: &Path) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(std::fs::read(p).unwrap())
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

/// Все файлы каталога данных (кроме `.zam.lock`, его содержимое — метка держателя): путь →
/// (размер, SHA-256).
fn state(root: &Path) -> BTreeMap<String, (u64, String)> {
    fn walk(d: &Path, root: &Path, out: &mut BTreeMap<String, (u64, String)>) {
        for e in std::fs::read_dir(d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, root, out);
                continue;
            }
            let rel = p
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if rel != ".zam.lock" {
                out.insert(rel, (std::fs::metadata(&p).unwrap().len(), sha_hex(&p)));
            }
        }
    }
    let mut m = BTreeMap::new();
    walk(root, root, &mut m);
    m
}

fn pre_dirs(root: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(root.join("backups"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("pre-rollback-")
        })
        .collect();
    v.sort();
    v
}

fn reindex_mark(p: &Path) -> Option<Value> {
    ro(p)
        .query_row(
            "SELECT value FROM meta WHERE key = 'reindex_pending'",
            [],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .map(|s| serde_json::from_str(&s).unwrap())
}

fn marker(root: &Path) -> PathBuf {
    root.join(zam_store::RESTORE_INCOMPLETE_FILE)
}

fn write_conf(dir: &Path, c: &ServerConfig) -> String {
    let conf = dir.join("zam.toml");
    std::fs::write(&conf, toml::to_string(c).unwrap()).unwrap();
    conf.to_string_lossy().into_owned()
}

fn zam_server(args: &[&str]) -> std::process::Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_zam-server"));
    c.args(args);
    for (k, _) in std::env::vars().filter(|(k, _)| k.starts_with("ZAM_")) {
        c.env_remove(k);
    }
    c.output().expect("запуск zam-server")
}

/// `--dry-run` печатает план (базы, версии, время и размеры копии, строка о потере записанного
/// после копии) и ничего не меняет — с `--yes` тоже. `v<N>` и `N` — одно и то же.
#[tokio::test]
async fn rollback_dry_run_changes_nothing() {
    assert_eq!(parse_version("v25"), Ok(25));
    assert_eq!(parse_version("25"), Ok(25));
    assert!(parse_version("x25").is_err());
    let fx = fixture("rb-dry", 2, 0, false).await;
    let before = state(&fx.root);
    for yes in [false, true] {
        let rep = rollback(&fx.cfg, &opts(25, None, true, yes)).unwrap();
        assert!(!rep.applied);
        assert_eq!(rep.plan.items.len(), 2);
        let text = rep.plan.text();
        for u in &fx.upgraded {
            assert!(text.contains(&format!("users/{u}.db")), "{text}");
        }
        assert!(text.contains(&format!("v{LATEST} → v25")), "{text}");
        assert!(text.contains("теряется"), "{text}");
        assert_eq!(state(&fx.root), before, "--dry-run, yes={yes}");
    }
    assert!(pre_dirs(&fx.root).is_empty());
}

/// Без `--yes` (и без `--dry-run`) — код 1, план в stdout, подсказка `--yes` в stderr, каталог не
/// тронут; `--dry-run` — код 0; `--json` — план в JSON.
#[tokio::test]
async fn rollback_requires_yes() {
    let fx = fixture("rb-yes", 1, 0, false).await;
    let conf = write_conf(fx.root.parent().unwrap(), &fx.cfg);
    let before = state(&fx.root);
    let out = zam_server(&["rollback", "--to", "v25", "--config", &conf]);
    let (stdout, stderr) = (
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert_eq!(out.status.code(), Some(1), "{stdout}\n{stderr}");
    assert!(
        stdout.contains(&format!("users/{}.db", fx.upgraded[0])),
        "{stdout}"
    );
    assert!(stderr.contains("--yes"), "{stderr}");
    assert_eq!(state(&fx.root), before);

    let out = zam_server(&[
        "rollback",
        "--to",
        "v25",
        "--dry-run",
        "--json",
        "--config",
        &conf,
    ]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let j: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(j["applied"], false);
    assert_eq!(j["plan"]["items"][0]["from"], LATEST);
    assert_eq!(j["plan"]["items"][0]["to"], 25);
    assert_eq!(state(&fx.root), before);
}

/// Порченая страница одной копии и чужая версия другой — отказ до первого изменения, обе причины
/// в тексте, каталог данных не тронут (ни переноса, ни признака).
#[tokio::test]
async fn rollback_bad_copy_changes_nothing() {
    let fx = fixture("rb-bad", 2, 0, false).await;
    let bak = |u: &Id| fx.root.join("users").join(format!("{u}.db.bak-v25"));
    {
        use std::io::{Seek, SeekFrom, Write};
        // страница 3 целиком (после VACUUM INTO свободных страниц нет — она занята): заголовок
        // b-дерева с недопустимым типом страницы
        let p = bak(&fx.upgraded[0]);
        assert!(std::fs::metadata(&p).unwrap().len() > 4 * 4096);
        let mut f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
        f.seek(SeekFrom::Start(2 * 4096)).unwrap();
        f.write_all(&[0xA5; 4096]).unwrap();
    }
    Connection::open(bak(&fx.upgraded[1]))
        .unwrap()
        .pragma_update(None, "user_version", 24)
        .unwrap();
    let before = state(&fx.root);
    match rollback(&fx.cfg, &opts(25, None, false, true)) {
        Err(e @ BackupError::RollbackRefused(_)) => {
            let text = e.to_string();
            for u in &fx.upgraded {
                assert!(text.contains(&format!("users/{u}.db.bak-v25")), "{text}");
            }
            assert!(text.contains("24"), "{text}");
        }
        other => panic!("ожидался RollbackRefused: {other:?}"),
    }
    assert_eq!(state(&fx.root), before);
    assert!(pre_dirs(&fx.root).is_empty());
    assert!(!marker(&fx.root).exists());
}

/// Текущая база с `-wal`/`-shm` (аварийная остановка) переносится в `backups/pre-rollback-…`
/// целиком — кадры WAL там же; на месте — копия без чужого WAL.
#[tokio::test]
async fn rollback_moves_current_with_wal_shm() {
    let fx = fixture("rb-wal", 1, 0, false).await;
    let p = fx.db(&fx.upgraded[0]);
    let side = |p: &Path, s: &str| PathBuf::from(format!("{}{s}", p.display()));
    let crash = fx.root.parent().unwrap().join("crash");
    std::fs::create_dir_all(&crash).unwrap();
    {
        let c = Connection::open(&p).unwrap();
        c.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
        c.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('probe', 'wal-only')",
            [],
        )
        .unwrap();
        for s in ["", "-wal", "-shm"] {
            std::fs::copy(side(&p, s), crash.join(format!("x.db{s}"))).unwrap();
        }
    }
    for s in ["", "-wal", "-shm"] {
        std::fs::copy(crash.join(format!("x.db{s}")), side(&p, s)).unwrap();
    }
    assert!(std::fs::metadata(side(&p, "-wal")).unwrap().len() > 0);
    let rep = rollback(&fx.cfg, &opts(25, None, false, true)).unwrap();
    assert!(rep.applied);
    let pre = rep.moved_to.clone().unwrap();
    let rel = format!("{}.db", fx.upgraded[0]);
    for s in ["", "-wal", "-shm"] {
        assert!(pre.join("users").join(format!("{rel}{s}")).is_file(), "{s}");
    }
    assert_eq!(probe(&pre.join("users").join(&rel)), "wal-only");
    assert_eq!(probe(&p), "v25");
    assert_eq!(version(&p), 25);
    assert!(!side(&p, "-wal").exists() || std::fs::metadata(side(&p, "-wal")).unwrap().len() == 0);
}

/// Копия `bak-v<N>` копируется, а не переносится: после отката она на месте и та же.
#[tokio::test]
async fn rollback_keeps_bak_copy() {
    let fx = fixture("rb-keep", 1, 0, false).await;
    let bak = fx
        .root
        .join("users")
        .join(format!("{}.db.bak-v25", fx.upgraded[0]));
    let sha = sha_hex(&bak);
    rollback(&fx.cfg, &opts(25, None, false, true)).unwrap();
    assert_eq!(sha_hex(&bak), sha);
    assert_eq!(probe(&fx.db(&fx.upgraded[0])), "v25");
}

/// Отметка «пересобрать индекс из кэша» — во всех базах пользователей (и в пропущенной), в
/// `qdrant/` — `.zam-reindex`.
#[tokio::test]
async fn rollback_marks_reindex_all() {
    let fx = fixture("rb-reindex", 2, 1, false).await;
    rollback(&fx.cfg, &opts(25, None, false, true)).unwrap();
    for u in fx.upgraded.iter().chain(&fx.at25) {
        assert_eq!(
            reindex_mark(&fx.db(u)),
            Some(json!({"full": false, "space": null})),
            "{u}"
        );
    }
    assert!(fx.root.join("qdrant").join(".zam-reindex").is_file());
}

/// `system.db` откатывается только с `--system v<M>`: без флага — та же; с флагом — копия
/// `bak-v6`, прежняя — в `pre-rollback-…`; нет копии — отказ.
#[tokio::test]
async fn rollback_system_only_with_flag() {
    let fx = fixture("rb-system", 1, 0, true).await;
    let sys = fx.root.join("system.db");
    rollback(&fx.cfg, &opts(25, None, false, true)).unwrap();
    assert_eq!(version(&sys), SYSTEM_MIGRATIONS.len() as u32);
    assert_eq!(sys_probe(&sys), "\"v7\"");
    assert!(!pre_dirs(&fx.root)[0].join("system.db").exists());

    assert!(matches!(
        rollback(&fx.cfg, &opts(25, Some(5), false, true)),
        Err(BackupError::RollbackRefused(_))
    ));
    let rep = rollback(&fx.cfg, &opts(25, Some(6), false, true)).unwrap();
    assert!(rep.applied);
    assert_eq!(version(&sys), 6);
    assert_eq!(sys_probe(&sys), "\"v6\"");
    let pre = rep.moved_to.clone().unwrap();
    assert_eq!(
        version(&pre.join("system.db")),
        SYSTEM_MIGRATIONS.len() as u32
    );
    assert!(rep.text().contains("system.db"));
}

/// База уже на версии N — пропуск строкой (не переносится); база ниже N — отказ до изменений.
#[tokio::test]
async fn rollback_same_version_skipped() {
    let fx = fixture("rb-same", 1, 1, false).await;
    let rep = rollback(&fx.cfg, &opts(25, None, false, true)).unwrap();
    let skipped = format!("users/{}.db", fx.at25[0]);
    assert!(
        rep.plan
            .skipped
            .iter()
            .any(|s| s.db == skipped && s.version == 25)
    );
    assert!(
        rep.plan.text().contains("уже на схеме v25"),
        "{}",
        rep.plan.text()
    );
    let pre = rep.moved_to.unwrap();
    assert!(!pre.join(&skipped).exists());
    assert_eq!(probe(&fx.db(&fx.at25[0])), "v25");

    let before = state(&fx.root);
    match rollback(&fx.cfg, &opts(26, None, false, true)) {
        Err(e @ BackupError::RollbackRefused(_)) => {
            assert!(e.to_string().contains("ниже"), "{e}");
        }
        other => panic!("ожидался RollbackRefused: {other:?}"),
    }
    assert_eq!(state(&fx.root), before);
}

/// Сервер запущен (лок каталога занят) — отказ с текстом про остановку сервера.
#[tokio::test]
async fn rollback_refuses_running_server() {
    let fx = fixture("rb-locked", 1, 0, false).await;
    let before = state(&fx.root);
    let lock = DataDir::open(&fx.root).unwrap();
    match rollback(&fx.cfg, &opts(25, None, false, true)) {
        Err(e @ BackupError::Locked) => assert!(e.to_string().contains("остановите сервер")),
        other => panic!("ожидался Locked: {other:?}"),
    }
    drop(lock);
    assert_eq!(state(&fx.root), before);
}

/// Сбой после переноса текущих баз: признак `rollback …` остаётся — сервер (`DataDir::open`) и
/// `backup` отказывают; прежние базы — в `pre-rollback-…`.
#[tokio::test]
async fn rollback_interrupted_blocks_serve() {
    let fx = fixture("rb-crash", 2, 0, false).await;
    test_hooks::set_fail_point(&fx.root, Some("rollback_moved"));
    let res = rollback(&fx.cfg, &opts(25, None, false, true));
    test_hooks::set_fail_point(&fx.root, None);
    let moved = match res {
        Err(BackupError::RollbackFailed { moved_to, .. }) => moved_to,
        other => panic!("ожидался RollbackFailed: {other:?}"),
    };
    let text = std::fs::read_to_string(marker(&fx.root)).unwrap();
    assert!(text.starts_with("rollback "), "{text}");
    assert!(matches!(
        DataDir::open(&fx.root),
        Err(StoreError::RollbackIncomplete(_))
    ));
    let Err(e) = zam_server::build(fx.cfg.clone(), Some("pw".into())).await else {
        panic!("сервер принял каталог прерванного отката");
    };
    assert!(e.to_string().contains("откат"), "{e}");
    let c = fx.cfg.clone();
    let r = tokio::task::spawn_blocking(move || backup(&c, &BackupOpts::default()))
        .await
        .unwrap();
    assert!(
        matches!(
            r,
            Err(BackupError::Store(StoreError::RollbackIncomplete(_)))
        ),
        "{r:?}"
    );
    for u in &fx.upgraded {
        assert!(!fx.db(u).exists());
        assert!(moved.join("users").join(format!("{u}.db")).is_file());
    }
}

/// Повтор той же команды после сбоя (одна база уже на месте) продолжает в тот же
/// `pre-rollback-…`: положенная база не переносится второй раз, недостающая копия кладётся,
/// признак снимается.
#[tokio::test]
async fn rollback_resume_after_crash() {
    let fx = fixture("rb-resume", 2, 0, false).await;
    test_hooks::set_fail_point(&fx.root, Some("rollback_placed"));
    let res = rollback(&fx.cfg, &opts(25, None, false, true));
    test_hooks::set_fail_point(&fx.root, None);
    let first = match res {
        Err(BackupError::RollbackFailed { moved_to, .. }) => moved_to,
        other => panic!("ожидался RollbackFailed: {other:?}"),
    };
    let placed: Vec<&Id> = fx.upgraded.iter().filter(|u| fx.db(u).exists()).collect();
    assert_eq!(placed.len(), 1, "сбой после первой положенной копии");
    let rep = rollback(&fx.cfg, &opts(25, None, false, true)).unwrap();
    assert!(rep.applied);
    assert_eq!(rep.moved_to.as_deref(), Some(first.as_path()));
    assert_eq!(pre_dirs(&fx.root), vec![first.clone()]);
    for u in &fx.upgraded {
        assert_eq!(version(&fx.db(u)), 25, "{u}");
        assert_eq!(probe(&fx.db(u)), "v25", "{u}");
        // в каталоге прежних данных — прежняя база, а не положенная копия
        assert_eq!(probe(&first.join("users").join(format!("{u}.db"))), "v27");
    }
    assert!(!marker(&fx.root).exists());
    drop(DataDir::open(&fx.root).unwrap());
}

/// M7.4 §7.1 п. 3: сумма миграции `system.db` расходится с программой — сервер не стартует,
/// текст называет миграцию и что делать.
#[tokio::test]
async fn serve_refuses_changed_system_migration() {
    let fx = fixture("rb-sysmig", 0, 0, false).await;
    Connection::open(fx.root.join("system.db"))
        .unwrap()
        .execute(
            "UPDATE zam_migrations SET sha256 = ?1 WHERE version = 3",
            ["0".repeat(64)],
        )
        .unwrap();
    let Err(e) = zam_server::build(fx.cfg.clone(), Some("pw".into())).await else {
        panic!("сервер стартовал с изменённой миграцией system.db");
    };
    let t = e.to_string();
    assert!(
        t.contains("миграция 3 system.db изменилась после применения к этой базе"),
        "{t}"
    );
    assert!(t.contains("подробности — zam-server doctor"), "{t}");
}

/// Аудит `server.rollback {to, system, dbs}` — в `system.db` после замены.
#[tokio::test]
async fn rollback_audit_recorded() {
    let fx = fixture("rb-audit", 2, 0, false).await;
    let rep = rollback(&fx.cfg, &opts(25, None, false, true)).unwrap();
    assert!(
        rep.text()
            .contains("замените файлы комплекта на выпуск со схемой v25"),
        "{}",
        rep.text()
    );
    let (actor, details): (String, String) = ro(&fx.root.join("system.db"))
        .query_row(
            "SELECT actor, details FROM audit WHERE action = 'server.rollback'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(actor, "cli");
    let d: Value = serde_json::from_str(&details).unwrap();
    assert_eq!(d["to"], 25);
    assert_eq!(d["system"], Value::Null);
    let dbs: Vec<String> = serde_json::from_value(d["dbs"].clone()).unwrap();
    let mut want: Vec<String> = fx
        .upgraded
        .iter()
        .map(|u| format!("users/{u}.db"))
        .collect();
    want.sort();
    assert_eq!(dbs, want);
}
