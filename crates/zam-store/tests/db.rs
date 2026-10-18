use std::path::PathBuf;

use zam_store::{Db, Migration, StoreError};

static MIGS: &[Migration] = &[
    Migration {
        version: 1,
        sql: "CREATE TABLE t(n INTEGER);",
    },
    Migration {
        version: 2,
        sql: "INSERT INTO t VALUES(0);",
    },
];

fn tmp(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("zam-store")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("x.db")
}

#[tokio::test]
async fn open_applies_pragmas_and_migrations() {
    let db = Db::open(&tmp("pragmas"), MIGS).unwrap();
    let (mode, fk, ver): (String, i64, i64) = db
        .read(|c| {
            Ok((
                c.query_row("PRAGMA journal_mode", [], |r| r.get(0))?,
                c.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?,
                c.query_row("PRAGMA user_version", [], |r| r.get(0))?,
            ))
        })
        .await
        .unwrap();
    assert_eq!((mode.as_str(), fk, ver), ("wal", 1, 2));
}

#[tokio::test]
async fn upgrade_makes_backup_copy() {
    let p = tmp("backup");
    Db::open(&p, &MIGS[..1]).unwrap();
    Db::open(&p, MIGS).unwrap();
    assert!(p.with_file_name("x.db.bak-v1").exists());
}

#[tokio::test]
async fn fresh_file_has_no_backup() {
    let p = tmp("nobackup");
    Db::open(&p, MIGS).unwrap();
    assert!(!p.with_file_name("x.db.bak-v0").exists());
}

#[tokio::test]
async fn newer_schema_is_refused() {
    let p = tmp("newer");
    Db::open(&p, MIGS).unwrap();
    match Db::open(&p, &MIGS[..1]) {
        Err(StoreError::SchemaTooNew {
            found: 2,
            supported: 1,
        }) => {}
        other => panic!("ожидали SchemaTooNew, получили {other:?}"),
    }
}

#[tokio::test]
async fn write_error_rolls_back() {
    let db = Db::open(&tmp("rollback"), MIGS).unwrap();
    let r: Result<(), _> = db
        .write(|tx| {
            tx.execute("INSERT INTO t VALUES(99)", [])?;
            Err(StoreError::Conflict("нет".into()))
        })
        .await;
    assert!(matches!(r, Err(StoreError::Conflict(_))));
    let cnt: i64 = db
        .read(|c| Ok(c.query_row("SELECT count(*) FROM t WHERE n=99", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(cnt, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_writes_are_serialized() {
    let db = Db::open(&tmp("concurrent"), MIGS).unwrap();
    let mut hs = Vec::new();
    for _ in 0..100 {
        let db = db.clone();
        hs.push(tokio::spawn(async move {
            db.write(|tx| Ok(tx.execute("UPDATE t SET n=n+1", [])?))
                .await
                .unwrap();
        }));
    }
    for h in hs {
        h.await.unwrap();
    }
    let n: i64 = db
        .read(|c| Ok(c.query_row("SELECT n FROM t", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(n, 100);
}

#[tokio::test]
async fn writer_survives_panic() {
    let db = Db::open(&tmp("panic"), MIGS).unwrap();
    let r: Result<(), _> = db
        .write(|tx| {
            tx.execute("UPDATE t SET n=5", [])?;
            panic!("тестовая паника");
        })
        .await;
    assert!(matches!(r, Err(StoreError::Closed)));
    db.write(|tx| Ok(tx.execute("UPDATE t SET n=n+1", [])?))
        .await
        .unwrap();
    let n: i64 = db
        .read(|c| Ok(c.query_row("SELECT n FROM t", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(n, 1, "панический UPDATE откатан");
}

#[tokio::test]
async fn readers_are_read_only_and_survive_panic() {
    let db = Db::open(&tmp("ro"), MIGS).unwrap();
    assert!(
        db.read(|c| Ok(c.execute("UPDATE t SET n=7", [])?))
            .await
            .is_err()
    );
    for _ in 0..8 {
        let db = db.clone();
        let _ = tokio::spawn(async move {
            db.read::<()>(|_| panic!("паника читателя")).await
        })
        .await;
    }
    // пул не опустел
    db.read(|c| Ok(c.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))?))
        .await
        .unwrap();
}

#[tokio::test]
async fn path_with_cyrillic_and_quote() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("zam-store")
        .join("кир 'и'ца");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("б д.db");
    Db::open(&p, &MIGS[..1]).unwrap();
    Db::open(&p, MIGS).unwrap();
    assert!(dir.join("б д.db.bak-v1").exists());
}

#[tokio::test]
async fn writer_thread_is_joined_on_drop() {
    let p = tmp("join");
    let db = Db::open(&p, MIGS).unwrap();
    db.write(|_| Ok(())).await.unwrap();
    drop(db);
    // На Windows rename падает, пока писатель держит файл открытым.
    std::fs::rename(&p, p.with_file_name("moved.db")).unwrap();
}

#[tokio::test]
async fn dangling_savepoint_does_not_break_next_write() {
    let db = Db::open(&tmp("savepoint"), MIGS).unwrap();
    db.write(|t| {
        t.execute_batch("SAVEPOINT x")?;
        Ok(())
    })
    .await
    .unwrap();
    db.write(|t| {
        t.execute("INSERT INTO t VALUES(5)", [])?;
        Ok(())
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn dangling_reader_transaction_is_rolled_back() {
    let db = Db::open(&tmp("rd_tx"), MIGS).unwrap();
    for _ in 0..8 {
        db.read(|c| {
            c.execute_batch("BEGIN")?;
            Ok(())
        })
        .await
        .unwrap();
    }
    let open: bool = db.read(|c| Ok(!c.is_autocommit())).await.unwrap();
    assert!(!open);
}

#[test]
fn invalid_migration_lists_are_rejected() {
    static DUP: &[Migration] = &[
        Migration {
            version: 1,
            sql: "CREATE TABLE a(x);",
        },
        Migration {
            version: 1,
            sql: "CREATE TABLE b(x);",
        },
    ];
    static GAP: &[Migration] = &[
        Migration {
            version: 1,
            sql: "CREATE TABLE a(x);",
        },
        Migration {
            version: 3,
            sql: "CREATE TABLE b(x);",
        },
    ];
    for (n, m) in [("dup", DUP), ("gap", GAP)] {
        let r = Db::open(&tmp(n), m);
        assert!(matches!(r, Err(StoreError::InvalidMigrations(_))), "{n}");
    }
}
