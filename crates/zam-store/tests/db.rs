use std::path::PathBuf;

use zam_store::{Db, DbOptions, Migration, StoreError};

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

#[tokio::test]
async fn drop_checkpoints_wal_and_data_survives_rename() {
    let p = tmp("ckpt");
    let db = Db::open(&p, MIGS).unwrap();
    for i in 0..200 {
        db.write(move |t| {
            t.execute("INSERT INTO t VALUES(?1)", [i])?;
            Ok(())
        })
        .await
        .unwrap();
    }
    // Читатели должны побывать в работе, иначе тест не проверяет порядок закрытия.
    db.read(|c| Ok(c.query_row("SELECT count(*) FROM t", [], |r| r.get::<_, i64>(0))?))
        .await
        .unwrap();
    drop(db);
    let mut wal = p.as_os_str().to_owned();
    wal.push("-wal");
    let wal = PathBuf::from(wal);
    assert!(!wal.exists() || std::fs::metadata(&wal).unwrap().len() == 0);
    let moved = p.with_file_name("moved.db");
    std::fs::rename(&p, &moved).unwrap();
    let db2 = Db::open(&moved, MIGS).unwrap();
    let n: i64 = db2
        .read(|c| Ok(c.query_row("SELECT count(*) FROM t", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(n, 201); // 200 + строка из миграции 2
}

// ---------- Checkpoint WAL вне потока-писателя (ТЗ §17) ----------

fn side_file(p: &std::path::Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

fn wal_len(p: &std::path::Path) -> u64 {
    std::fs::metadata(side_file(p, "-wal"))
        .map(|m| m.len())
        .unwrap_or(0)
}

/// Номер checkpoint в заголовке WAL (смещение 12): растёт при каждом сбросе WAL
/// на начало файла; 0 — файла нет или WAL ещё не сбрасывался.
fn wal_seq(p: &std::path::Path) -> u32 {
    use std::io::Read;
    let mut h = [0u8; 16];
    std::fs::File::open(side_file(p, "-wal"))
        .and_then(|mut f| f.read_exact(&mut h))
        .map(|()| u32::from_be_bytes([h[12], h[13], h[14], h[15]]))
        .unwrap_or(0)
}

static BLOB_MIGS: &[Migration] = &[Migration {
    version: 1,
    sql: "CREATE TABLE b(x BLOB);",
}];

async fn write_blobs(db: &Db, n: usize, size: usize) {
    for _ in 0..n {
        db.write(move |t| {
            t.execute("INSERT INTO b VALUES(zeroblob(?1))", [size as i64])?;
            Ok(())
        })
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn writer_does_not_autocheckpoint() {
    let db = Db::open(&tmp("noauto"), BLOB_MIGS).unwrap();
    let n: i64 = db
        .write(|t| Ok(t.query_row("PRAGMA wal_autocheckpoint", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(n, 0);
}

#[tokio::test]
async fn wal_bounded_by_writer_safety_net_without_background() {
    let p = tmp("safety");
    let limit = 512 * 1024;
    let opts = DbOptions {
        background_checkpoint: false,
        wal_limit_bytes: limit,
        ..DbOptions::default()
    };
    let db = Db::open_with(&p, BLOB_MIGS, opts).unwrap();
    // 8 МБ записей при пределе WAL 512 КиБ
    write_blobs(&db, 256, 32 * 1024).await;
    let len = wal_len(&p);
    assert!(len <= 2 * limit, "WAL {len} байт при пределе {limit}");
    drop(db);
}

#[tokio::test]
async fn background_checkpointer_keeps_wal_small() {
    let p = tmp("bg");
    let opts = DbOptions {
        checkpoint_interval: std::time::Duration::from_millis(50),
        ..DbOptions::default()
    };
    let db = Db::open_with(&p, BLOB_MIGS, opts).unwrap();
    let db_len = || std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
    // пять порций по 1 МБ; после каждой ждём, пока фоновый поток перенесёт кадры
    // в файл базы (без checkpoint файл базы не растёт) и файл перестанет меняться
    for k in 1..=5u64 {
        write_blobs(&db, 32, 32 * 1024).await;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let (mut last, mut stable_since) = (0, std::time::Instant::now());
        loop {
            let now = db_len();
            if now != last {
                (last, stable_since) = (now, std::time::Instant::now());
            }
            if now >= k * 1024 * 1024 && stable_since.elapsed().as_millis() >= 500 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "фоновый checkpoint не перенёс порцию {k}: база {now} байт"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    let (len, seq) = (wal_len(&p), wal_seq(&p));
    assert!(seq >= 1, "WAL ни разу не начат сначала");
    // без перезапусков WAL был бы больше 5 МБ (см. without_any_checkpoint_wal_grows)
    assert!(len < 5 * 1024 * 1024, "WAL {len} байт, перезапусков {seq}");
    eprintln!("WAL {len} байт, перезапусков {seq}");
}

#[tokio::test]
async fn drop_leaves_no_wal_and_shm() {
    let p = tmp("close");
    let db = Db::open(&p, BLOB_MIGS).unwrap();
    write_blobs(&db, 64, 16 * 1024).await;
    db.read(|c| Ok(c.query_row("SELECT count(*) FROM b", [], |r| r.get::<_, i64>(0))?))
        .await
        .unwrap();
    drop(db);
    assert!(!side_file(&p, "-wal").exists(), "-wal остался");
    assert!(!side_file(&p, "-shm").exists(), "-shm остался");
}

#[tokio::test]
async fn without_any_checkpoint_wal_grows() {
    // контроль к двум тестам выше: без фона и с большим пределом WAL растёт
    let p = tmp("grow");
    let opts = DbOptions {
        background_checkpoint: false,
        wal_limit_bytes: u64::MAX,
        ..DbOptions::default()
    };
    let db = Db::open_with(&p, BLOB_MIGS, opts).unwrap();
    write_blobs(&db, 160, 32 * 1024).await;
    let len = wal_len(&p);
    assert!(len >= 5 * 1024 * 1024, "WAL {len} байт");
}

#[tokio::test]
async fn background_restart_resets_wal_under_continuous_writes() {
    // страховка писателя выключена (предел u64::MAX): сбросить WAL может только
    // фоновый RESTART. Без него при записи без пауз WAL не начинается сначала ни
    // разу (номер checkpoint в заголовке остаётся 0, файл растёт на весь объём).
    let p = tmp("bg-restart");
    let opts = DbOptions {
        checkpoint_interval: std::time::Duration::from_millis(20),
        wal_limit_bytes: u64::MAX,
        wal_restart_bytes: 256 * 1024,
        ..DbOptions::default()
    };
    let db = Db::open_with(&p, BLOB_MIGS, opts).unwrap();
    // до 64 МБ записей по 32 КБ с паузой 1 мс, пока WAL не начнётся сначала
    let mut writes = 0;
    while wal_seq(&p) == 0 && writes < 2048 {
        write_blobs(&db, 1, 32 * 1024).await;
        writes += 1;
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(wal_seq(&p) >= 1, "WAL не сброшен за {writes} записей");
}
