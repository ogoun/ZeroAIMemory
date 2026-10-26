//! Перенарезка по отпечатку нарезки (задача 17 M3, fix round 1): записи журналов
//! перенарезаются вместе с заметками, а прерванная перенарезка отпечаток не ставит и
//! повторяется при следующем открытии базы.
//!
//! Отдельный тестовый бинарник: пороги чанкера глобальные на процесс, а `rechunk_chunker.rs`
//! их меняет. Здесь — умолчания библиотеки.

use std::path::PathBuf;

use jiff::Timestamp;
use serde_json::json;
use zam_core::Id;
use zam_store::{
    Attribution, AuthorKind, Certainty, DataDir, Db, EntryKind, NewEntry, NewNote, NodePath,
    SpaceKind, UserDbs, claim_jobs, complete_job, create_entry, create_note, list_spaces,
};

async fn count(db: &Db, sql: &'static str) -> i64 {
    db.read(move |c| Ok(c.query_row(sql, [], |r| r.get(0))?))
        .await
        .unwrap()
}

/// `(ordinal, embed_key)` чанков записей журналов.
async fn entry_keys(db: &Db) -> Vec<(i64, String)> {
    db.read(|c| {
        let mut st = c.prepare(
            "SELECT ordinal, embed_key FROM chunks WHERE entry_id IS NOT NULL
             ORDER BY entry_id, level, ordinal",
        )?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    })
    .await
    .unwrap()
}

async fn drain(db: &Db) {
    for j in claim_jobs(db, 100, Timestamp::now()).await.unwrap() {
        complete_job(db, j.id, j.claim_gen).await.unwrap();
    }
}

async fn exec(db: &Db, sql: &'static str) {
    db.write(move |tx| {
        tx.execute_batch(sql)?;
        Ok(())
    })
    .await
    .unwrap();
}

async fn reopen(dd: &DataDir, user: Id) -> Db {
    let dbs = UserDbs::new(dd.users_dir());
    let db = dbs.get(user).await.unwrap();
    dbs.wait_rechunk(user).await;
    db
}

const FINGERPRINT: &str = "SELECT count(*) FROM meta WHERE key = 'chunker'";
const QUEUED_ENTRY: &str = "SELECT count(*) FROM index_jobs
     WHERE target_kind = 'entry' AND state = 'queued'";
const STALE_ENTRY: &str = "SELECT count(*) FROM chunks
     WHERE entry_id IS NOT NULL AND embed_key = 'старый'";

#[tokio::test]
async fn interrupted_rechunk_retried_and_entries_rechunked() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("rechunk-chunker-retry");
    let _ = std::fs::remove_dir_all(&dir);
    let dd = DataDir::open(&dir).unwrap();
    let user = Id::new();

    let db = UserDbs::new(dd.users_dir()).get(user).await.unwrap();
    let k = list_spaces(&db)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.kind == SpaceKind::Knowledge)
        .unwrap()
        .id;
    let attrib = Attribution::new(AuthorKind::Agent);
    let para = "Перенос данных идёт пакетами, каждый пакет проверяется. ".repeat(30);
    create_note(
        &db,
        NewNote {
            space_id: k,
            path: NodePath::parse("перенос").unwrap(),
            content: format!("# Перенос\n\n{para}\n"),
            l0: None,
            attrib: attrib.clone(),
        },
    )
    .await
    .unwrap();
    create_entry(
        &db,
        NewEntry {
            space_id: k,
            kind: EntryKind::Insight,
            title: "Пакеты".into(),
            body: format!("Наблюдение по переносу. {para}"),
            data: json!({"kind": "gotcha"}),
            area: None,
            tags: Vec::new(),
            status: None,
            certainty: Some(Certainty::Hypothesis),
            idempotency_key: None,
            triggers: Vec::new(),
            links: Vec::new(),
        },
        &attrib,
    )
    .await
    .unwrap();
    drain(&db).await;
    assert_eq!(count(&db, FINGERPRINT).await, 1);
    let keys_now = entry_keys(&db).await;
    assert!(!keys_now.is_empty());

    // 1. База «до задачи 17» (отпечатков заметок и записей нет), чанки записи устарели, а
    // запись чанков записей падает: перенарезка прерывается на записях — отпечаток не ставится.
    exec(
        &db,
        "DELETE FROM meta WHERE key IN ('chunker', 'chunker_entries');
         UPDATE chunks SET embed_key = 'старый' WHERE entry_id IS NOT NULL;
         CREATE TRIGGER fail_entry_chunks BEFORE INSERT ON chunks
         WHEN NEW.entry_id IS NOT NULL
         BEGIN SELECT RAISE(ABORT, 'сбой записи чанков'); END;",
    )
    .await;
    drop(db);
    let db = reopen(&dd, user).await;
    assert_eq!(count(&db, FINGERPRINT).await, 0, "прервана — без отпечатка");
    assert_eq!(
        count(&db, STALE_ENTRY).await,
        keys_now.len() as i64,
        "пакет записей откатился"
    );
    exec(&db, "DROP TRIGGER fail_entry_chunks;").await;
    drain(&db).await;
    drop(db);

    // 2. Следующее открытие повторяет перенарезку: записи перенарезаны (embed_key — по
    // тексту, как до порчи), на них поставлен upsert, отпечаток записан.
    let db = reopen(&dd, user).await;
    assert_eq!(count(&db, FINGERPRINT).await, 1, "отпечаток после успеха");
    assert_eq!(count(&db, STALE_ENTRY).await, 0);
    assert_eq!(entry_keys(&db).await, keys_now);
    assert!(count(&db, QUEUED_ENTRY).await >= 1, "upsert записи");
    drain(&db).await;
    drop(db);

    // 3. И больше не повторяется.
    let db = reopen(&dd, user).await;
    assert_eq!(count(&db, QUEUED_ENTRY).await, 0);
}
