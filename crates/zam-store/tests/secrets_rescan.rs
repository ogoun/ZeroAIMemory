//! Разовая перенарезка заметок и записей со значениями секретов при первом открытии базы,
//! созданной до сканера (§7.6, задача 11 M3): значения уходят из `chunks` (и FTS), ставятся
//! задания `upsert`, повторное открытие ничего не делает. С задачи 15 — теги в payload точек
//! и вход эмбеддинга папки тоже без значений (`secrets_scanned_v3`: пересборка из кэша).

use std::path::PathBuf;

use jiff::Timestamp;
use serde_json::json;
use zam_core::Id;
use zam_markdown::SECRET_MARK;
use zam_store::{
    Attribution, AuthorKind, Certainty, DataDir, Db, EntryKind, NewEntry, NewNote, NodePath,
    SpaceKind, UserDbs, chunks_for_index, claim_jobs, complete_job, create_entry, create_note,
    entry_chunks_for_index, folder_embed_input, job_stats, list_spaces, mark_reindex,
    pending_reindex,
};

const FLAG: &str = "secrets_scanned_v2";

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
            refs: Vec::new(),
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

/// Флаг разовой пересборки payload и точек папок (теги и вход эмбеддинга папки, §7.6).
const FLAG_V3: &str = "secrets_scanned_v3";

#[tokio::test]
async fn payload_tags_and_folder_input_masked_old_db_reindexed_once() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("secrets-payload");
    let _ = std::fs::remove_dir_all(&dir);
    let dd = DataDir::open(&dir).unwrap();
    let (user, clean_user) = (Id::new(), Id::new());
    let s = secret();

    let db = UserDbs::new(dd.users_dir()).get(user).await.unwrap();
    // пустая новая база: флаг ставится сразу, пересборки нет
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM meta WHERE key = ?1",
            FLAG_V3.into()
        )
        .await,
        1
    );
    assert!(pending_reindex(&db).await.unwrap().is_none());
    let k = list_spaces(&db)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.kind == SpaceKind::Knowledge)
        .unwrap()
        .id;
    let attrib = Attribution::new(AuthorKind::Agent);
    let note = create_note(
        &db,
        NewNote {
            space_id: k,
            path: NodePath::parse("ops/теги").unwrap(),
            content: format!("---\ntags: [deploy, {s}]\n---\n# Теги\n\nтолько текст\n"),
            l0: None,
            attrib: attrib.clone(),
        },
    )
    .await
    .unwrap();
    let (entry, _) = create_entry(
        &db,
        NewEntry {
            refs: Vec::new(),
            space_id: k,
            kind: EntryKind::Insight,
            title: "Теги записи".into(),
            body: "текст без секретов".into(),
            data: json!({"kind": "gotcha"}),
            area: None,
            tags: vec!["ops".into(), s.clone()],
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
    // теги payload точек Qdrant — без значений секретов (остальные теги как есть)
    let chunks = chunks_for_index(&db, note.id).await.unwrap();
    assert!(!chunks.is_empty());
    for c in &chunks {
        assert!(
            !c.node.tags.iter().any(|t| t.contains(&s)),
            "{:?}",
            c.node.tags
        );
        assert!(c.node.tags.contains(&"deploy".to_string()));
        assert!(c.node.tags.iter().any(|t| t.contains(SECRET_MARK)));
    }
    let echunks = entry_chunks_for_index(&db, entry.id).await.unwrap();
    assert!(!echunks.is_empty());
    for c in &echunks {
        assert!(
            !c.node.tags.iter().any(|t| t.contains(&s)),
            "{:?}",
            c.node.tags
        );
        assert!(c.node.tags.contains(&"ops".to_string()));
    }

    // имитация базы до задачи 15: флага нет, L0 папки (свёртка детей) несёт значение
    let old = format!("- теги: ключ {s}");
    db.write(move |tx| {
        tx.execute("DELETE FROM meta WHERE key = ?1", [FLAG_V3])?;
        tx.execute(
            "UPDATE nodes SET l0 = ?1 WHERE kind = 'folder' AND path = 'ops'",
            [old],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    drop(db);

    // открытие: L0 папки замаскирован, отмечена пересборка индекса базы из кэша (точки
    // заметок и записей получат замаскированные теги, папки — новый вход эмбеддинга)
    let dbs = UserDbs::new(dd.users_dir());
    let db = dbs.get(user).await.unwrap();
    let spec = pending_reindex(&db)
        .await
        .unwrap()
        .expect("отметка reindex");
    let v: serde_json::Value = serde_json::from_str(&spec).unwrap();
    assert_eq!(v["full"], false, "{spec}");
    assert!(v["space"].is_null(), "{spec}");
    let like = format!("%{s}%");
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM nodes WHERE kind = 'folder' AND l0 LIKE ?1",
            like
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM meta WHERE key = ?1",
            FLAG_V3.into()
        )
        .await,
        1
    );
    // повторное открытие ничего не отмечает
    mark_reindex(&db, None).await.unwrap();
    drop((db, dbs));
    let db = UserDbs::new(dd.users_dir()).get(user).await.unwrap();
    assert!(pending_reindex(&db).await.unwrap().is_none());

    // старая база без секретов в тегах и папках: флаг ставится, пересборки нет
    let db = UserDbs::new(dd.users_dir()).get(clean_user).await.unwrap();
    let k = list_spaces(&db)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.kind == SpaceKind::Knowledge)
        .unwrap()
        .id;
    create_note(
        &db,
        NewNote {
            space_id: k,
            path: NodePath::parse("ops/чисто").unwrap(),
            content: "---\ntags: [deploy]\n---\n# Чисто\n\nтекст\n".into(),
            l0: None,
            attrib,
        },
    )
    .await
    .unwrap();
    db.write(|tx| {
        tx.execute("DELETE FROM meta WHERE key = ?1", [FLAG_V3])?;
        tx.execute(
            "UPDATE nodes SET l0 = '- чисто: текст' WHERE kind = 'folder'",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    drop(db);
    let db = UserDbs::new(dd.users_dir()).get(clean_user).await.unwrap();
    assert!(pending_reindex(&db).await.unwrap().is_none());
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM meta WHERE key = ?1",
            FLAG_V3.into()
        )
        .await,
        1
    );
}

#[test]
fn folder_embed_input_masks_secrets_and_keeps_clean_text() {
    let s = secret();
    // без секретов — прежняя строка (ключ кэша чистых папок не меняется)
    assert_eq!(
        folder_embed_input("Knowledge", "ops/deploy", "deploy", "- a: b"),
        "[Knowledge/ops/deploy] deploy\n- a: b"
    );
    for (path, name, l0) in [
        (format!("ops/{s}"), s.clone(), "- a: b".to_string()),
        ("ops".to_string(), "ops".to_string(), format!("- ключ: {s}")),
    ] {
        let input = folder_embed_input("Knowledge", &path, &name, &l0);
        assert!(!input.contains(&s), "{input}");
        assert!(input.contains(SECRET_MARK), "{input}");
    }
}

/// Уже стоящая частичная отметка reindex (прерванный reindex одного пространства) при
/// дозаполнении v3 расширяется до всей базы с сохранением режима: иначе сырые теги в других
/// пространствах остались бы в Qdrant (финальная волна M3, ревью задачи 15).
#[tokio::test]
async fn payload_rescan_widens_partial_reindex_mark() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("secrets-payload-widen");
    let _ = std::fs::remove_dir_all(&dir);
    let dd = DataDir::open(&dir).unwrap();
    let user = Id::new();
    let s = secret();
    let db = UserDbs::new(dd.users_dir()).get(user).await.unwrap();
    let k = list_spaces(&db)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.kind == SpaceKind::Knowledge)
        .unwrap()
        .id;
    create_note(
        &db,
        NewNote {
            space_id: k,
            path: NodePath::parse("теги").unwrap(),
            content: format!("---\ntags: [{s}]\n---\n# Теги\n\nтекст\n"),
            l0: None,
            attrib: Attribution::new(AuthorKind::Agent),
        },
    )
    .await
    .unwrap();
    let other = Id::new().to_string();
    db.write(move |tx| {
        tx.execute("DELETE FROM meta WHERE key = ?1", [FLAG_V3])?;
        Ok(())
    })
    .await
    .unwrap();
    mark_reindex(&db, Some(json!({"full": true, "space": other}).to_string()))
        .await
        .unwrap();
    drop(db);
    let db = UserDbs::new(dd.users_dir()).get(user).await.unwrap();
    let spec = pending_reindex(&db).await.unwrap().expect("отметка");
    let v: serde_json::Value = serde_json::from_str(&spec).unwrap();
    assert!(v["space"].is_null(), "вся база: {spec}");
    assert_eq!(v["full"], true, "режим прежней отметки: {spec}");
}
