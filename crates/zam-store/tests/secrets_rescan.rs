//! Разовая перенарезка заметок и записей со значениями секретов при первом открытии базы,
//! созданной до сканера (§7.6, задача 11 M3): значения уходят из `chunks` (и FTS), ставятся
//! задания `upsert`, повторное открытие ничего не делает.

use std::path::PathBuf;

use jiff::Timestamp;
use serde_json::json;
use zam_core::Id;
use zam_store::{
    Attribution, AuthorKind, Certainty, DataDir, Db, EntryKind, NewEntry, NewNote, NodePath,
    SpaceKind, UserDbs, claim_jobs, complete_job, create_entry, create_note, job_stats,
    list_spaces,
};

const FLAG: &str = "secrets_scanned_v1";

fn secret() -> String {
    format!("gh{}_{}", "p", "aB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1eF3hJ5")
}

async fn count(db: &Db, sql: &'static str, arg: String) -> i64 {
    db.read(move |c| Ok(c.query_row(sql, [arg], |r| r.get(0))?))
        .await
        .unwrap()
}

async fn drain(db: &Db) {
    for j in claim_jobs(db, 100, Timestamp::now()).await.unwrap() {
        complete_job(db, j.id, j.claim_gen).await.unwrap();
    }
}

#[tokio::test]
async fn old_db_with_secrets_is_rescanned_once() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("secrets-rescan");
    let _ = std::fs::remove_dir_all(&dir);
    let dd = DataDir::open(&dir).unwrap();
    let user = Id::new();
    let s = secret();

    // пустая новая база: флаг ставится сразу
    let db = UserDbs::new(dd.users_dir()).get(user).await.unwrap();
    assert_eq!(
        count(&db, "SELECT count(*) FROM meta WHERE key = ?1", FLAG.into()).await,
        1
    );
    let k = list_spaces(&db)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.kind == SpaceKind::Knowledge)
        .unwrap()
        .id;
    let attrib = Attribution::new(AuthorKind::Agent);
    create_note(
        &db,
        NewNote {
            space_id: k,
            path: NodePath::parse("ops/ключи").unwrap(),
            content: format!("# Ключи\n\nGITHUB_TOKEN={s}\n"),
            l0: None,
            attrib: attrib.clone(),
        },
    )
    .await
    .unwrap();
    create_note(
        &db,
        NewNote {
            space_id: k,
            path: NodePath::parse("ops/чисто").unwrap(),
            content: "# Чисто\n\nтолько текст\n".into(),
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
            title: "Утечка".into(),
            body: format!("В логе был password = {s}"),
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
    let like = format!("%{s}%");
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM chunks WHERE text LIKE ?1",
            like.clone()
        )
        .await,
        0,
        "новые записи нарезаются замаскированными"
    );

    // имитация базы M2: значения в чанках, флага нет
    let old = s.clone();
    db.write(move |tx| {
        tx.execute("DELETE FROM meta WHERE key = ?1", [FLAG])?;
        tx.execute(
            "UPDATE chunks SET text = replace(text, '‹secret›', ?1) WHERE text LIKE '%‹secret›%'",
            [old],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    assert!(
        count(
            &db,
            "SELECT count(*) FROM chunks WHERE text LIKE ?1",
            like.clone()
        )
        .await
            > 0
    );
    drain(&db).await;
    drop(db);

    // первое открытие: фоновая перенарезка только затронутых заметок и записей
    let dbs = UserDbs::new(dd.users_dir());
    let db = dbs.get(user).await.unwrap();
    dbs.wait_rechunk(user).await;
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM chunks WHERE text LIKE ?1",
            like.clone()
        )
        .await,
        0,
        "значения убраны из chunks"
    );
    assert!(
        count(
            &db,
            "SELECT count(*) FROM chunks WHERE text LIKE ?1",
            "%‹secret›%".into()
        )
        .await
            > 0
    );
    assert_eq!(
        count(&db, "SELECT count(*) FROM meta WHERE key = ?1", FLAG.into()).await,
        1
    );
    assert_eq!(
        job_stats(&db).await.unwrap().queued,
        2,
        "upsert заметки с секретом и записи; чистая заметка не тронута"
    );
    drain(&db).await;
    drop((db, dbs));

    // повторное открытие ничего не делает
    let dbs = UserDbs::new(dd.users_dir());
    let db = dbs.get(user).await.unwrap();
    dbs.wait_rechunk(user).await;
    assert_eq!(job_stats(&db).await.unwrap().queued, 0);
}
