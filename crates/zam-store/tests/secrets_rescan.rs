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
    loop {
        let jobs = claim_jobs(db, 100, Timestamp::now()).await.unwrap();
        if jobs.is_empty() {
            break;
        }
        for j in jobs {
            complete_job(db, j.id, j.claim_gen).await.unwrap();
        }
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
            refs: None,
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
            refs: None,
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

async fn meta_value(db: &Db, key: &'static str) -> String {
    db.read(move |c| Ok(c.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))?))
        .await
        .unwrap()
}

/// Совпадений значения в FTS чанков (`chunks_fts`, фраза).
async fn fts_hits(db: &Db, value: &str) -> i64 {
    let q = format!("\"{value}\"");
    db.read(move |c| {
        Ok(c.query_row(
            "SELECT count(*) FROM chunks_fts WHERE chunks_fts MATCH ?1",
            [q],
            |r| r.get(0),
        )?)
    })
    .await
    .unwrap()
}

/// Ревью 15b, N4: заметка без тела с секретом в имени — чанк-заголовок замаскирован при
/// записи, и разностная перепроверка (флаги `'1'`) его не находит повторно.
#[tokio::test]
async fn bodyless_note_with_secret_name_masked_and_not_refound() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("secrets-bodyless-name");
    let _ = std::fs::remove_dir_all(&dir);
    let dd = DataDir::open(&dir).unwrap();
    let user = Id::new();
    let value = format!("Sup3r{}", "S3cretValue1");
    let db = UserDbs::new(dd.users_dir()).get(user).await.unwrap();
    let k = knowledge(&db).await;
    let n = note_in(&db, k, &format!("ops/password={value}"), String::new()).await;
    let like = format!("%{value}%");
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM chunks WHERE text LIKE ?1 OR heading_path LIKE ?1",
            like.clone()
        )
        .await,
        0,
        "имя в чанке-заголовке замаскировано"
    );
    assert!(
        count(
            &db,
            "SELECT count(*) FROM chunks WHERE node_id = ?1",
            n.to_string()
        )
        .await
            > 0,
        "чанк-заголовок есть"
    );
    db.write(|tx| {
        tx.execute(
            "UPDATE meta SET value = '1' WHERE key IN (?1, ?2)",
            [FLAG, FLAG_V3],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    drain(&db).await;
    drop(db);
    let dbs = UserDbs::new(dd.users_dir());
    let db = dbs.get(user).await.unwrap();
    dbs.wait_rechunk(user).await;
    assert_eq!(
        job_of(&db, "node", n).await,
        None,
        "перепроверка не находит заметку снова"
    );
}

/// Цель задания в очереди (`target_kind`, `op`) или `None`.
async fn job_of(db: &Db, kind: &'static str, id: Id) -> Option<String> {
    db.read(move |c| {
        use rusqlite::OptionalExtension;
        Ok(c.query_row(
            "SELECT op FROM index_jobs
             WHERE target_kind = ?1 AND target_id = ?2 AND state IN ('queued', 'running')",
            rusqlite::params![kind, id],
            |r| r.get(0),
        )
        .optional()?)
    })
    .await
    .unwrap()
}

async fn knowledge(db: &Db) -> Id {
    list_spaces(db)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.kind == SpaceKind::Knowledge)
        .unwrap()
        .id
}

async fn note_in(db: &Db, k: Id, path: &str, content: String) -> Id {
    create_note(
        db,
        NewNote {
            space_id: k,
            path: NodePath::parse(path).unwrap(),
            content,
            l0: None,
            attrib: Attribution::new(AuthorKind::Agent),
        },
    )
    .await
    .unwrap()
    .id
}

/// Ревью задачи 15b M6 (I1, повторное ревью I2, M2, M5): правило сканера расширено (значение в
/// обратных кавычках) — база, проверенная прежними правилами (`secrets_scanned_v2/v3 = '1'`),
/// перепроверяется **по сохранённому**: найденное текущими правилами в чанках заметки, записи,
/// вложения, в L0 папки и в сигнатуре символа переписывается и получает задание; заметка со
/// «старым», правильно замаскированным секретом не трогается, отметки reindex нет; флаги —
/// текущая версия; повторное открытие ничего не делает.
#[tokio::test]
async fn rules_version_bump_rescans_old_db_once() {
    use zam_store::attachments::{
        DocIntake, DocKind, ExtractState, Locator, Unit, list_attachments,
        replace_attachment_chunks, set_extract_state,
    };
    use zam_store::blobs::{BlobStore, attach};
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("secrets-rules-bump");
    let _ = std::fs::remove_dir_all(&dir);
    let dd = DataDir::open(&dir).unwrap();
    let user = Id::new();
    let pw = format!("Sup3r{}", "S3cretValue");
    let db = UserDbs::new(dd.users_dir()).get(user).await.unwrap();
    let k = knowledge(&db).await;
    let attrib = Attribution::new(AuthorKind::Agent);
    // новая утечка: значение в обратных кавычках в заметке, записи, вложении, L0 папки, сигнатуре
    let leaky = note_in(&db, k, "ops/db", format!("# База\n\nDB_PASSWORD=`{pw}`\n")).await;
    // старый секрет, замаскированный прежними правилами: не трогается
    let masked = note_in(
        &db,
        k,
        "ops/old",
        format!("# Старое\n\nGITHUB_TOKEN={}\n", secret()),
    )
    .await;
    let clean_in_folder = note_in(&db, k, "docs/clean", "# Чисто\n\nтекст\n".into()).await;
    let entry = create_entry(
        &db,
        NewEntry {
            refs: None,
            space_id: k,
            kind: EntryKind::Insight,
            title: "Пароль в логе".into(),
            body: format!("в логе было password: `{pw}`"),
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
    .unwrap()
    .0
    .id;
    let bs = BlobStore::new(&dd.blobs_dir(), user);
    let (sha, size) = bs.put(&b"doc"[..], 1 << 20).await.unwrap();
    attach(
        &db,
        clean_in_folder,
        &sha,
        size,
        "creds.txt",
        "text/plain",
        DocIntake::classify(Some(DocKind::Text), size, u64::MAX),
        AuthorKind::Human,
    )
    .await
    .unwrap();
    let doc = list_attachments(&db, clean_in_folder).await.unwrap()[0].clone();
    replace_attachment_chunks(
        &db,
        doc.id,
        &[Unit {
            locator: Locator::Para(1),
            heading_path: Vec::new(),
            text: format!("token = `{pw}`"),
        }],
    )
    .await
    .unwrap();
    set_extract_state(&db, doc.id, ExtractState::Done, None)
        .await
        .unwrap();
    let like = format!("%{pw}%");
    let open_chunks = |db: Db| {
        let like = like.clone();
        async move { count(&db, "SELECT count(*) FROM chunks WHERE text LIKE ?1", like).await }
    };
    assert_eq!(
        open_chunks(db.clone()).await,
        0,
        "новые записи — замаскированы"
    );
    let version = zam_markdown::secrets::RULES_VERSION.to_string();
    for flag in [FLAG, FLAG_V3] {
        assert_eq!(meta_value(&db, flag).await, version, "{flag}");
    }

    // имитация базы, проверенной прежними правилами: флаги '1', новое значение в сохранённом
    // открыто (у «старого» секрета маска остаётся)
    let old = pw.clone();
    let folder_l0 = format!("ключи: pwd=`{pw}`");
    db.write(move |tx| {
        tx.execute(
            "UPDATE meta SET value = '1' WHERE key IN (?1, ?2)",
            [FLAG, FLAG_V3],
        )?;
        let mark = format!("`{}`", zam_markdown::SECRET_MARK);
        tx.execute(
            "UPDATE chunks SET text = replace(text, ?2, ?1) WHERE instr(text, ?2) > 0",
            [format!("`{old}`"), mark],
        )?;
        tx.execute(
            "UPDATE nodes SET l0 = ?1 WHERE kind = 'folder' AND path = 'docs'",
            [folder_l0],
        )?;
        tx.execute(
            "INSERT INTO code_snapshots (solution_guid, updated_at) VALUES ('g', 't')",
            [],
        )?;
        tx.execute(
            "INSERT INTO code_files (id, solution_guid, path, lang, content_hash, size,
                                     parse_state, updated_at)
             VALUES (1, 'g', 'src/db.rs', 'rust', 'h', 1, 'ok', 't')",
            [],
        )?;
        tx.execute(
            "INSERT INTO code_symbols (id, file_id, name, qual, kind, line, end_line, sig,
                                       body_hash)
             VALUES (1, 1, 'connect', 'connect', 'fn', 1, 2, ?1, 'b'),
                    (2, 1, 'plain', 'plain', 'fn', 3, 4, 'fn plain(x: u32)', 'b')",
            [format!("fn connect(password = `{old}`)")],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
        open_chunks(db.clone()).await,
        3,
        "заметка, запись, вложение"
    );
    drain(&db).await;
    drop(db);

    let dbs = UserDbs::new(dd.users_dir());
    let db = dbs.get(user).await.unwrap();
    dbs.wait_rechunk(user).await;
    assert_eq!(
        open_chunks(db.clone()).await,
        0,
        "значение убрано из chunks заметки, записи и (на месте, N1) вложения"
    );
    assert_eq!(fts_hits(&db, &pw).await, 0, "и из FTS");
    assert_eq!(job_of(&db, "node", leaky).await.as_deref(), Some("upsert"));
    assert_eq!(job_of(&db, "entry", entry).await.as_deref(), Some("upsert"));
    assert_eq!(
        job_of(&db, "node", masked).await,
        None,
        "старый секрет не трогается"
    );
    assert_eq!(
        job_of(&db, "node", clean_in_folder).await.as_deref(),
        Some("upsert"),
        "свёртка папки с утечкой в L0"
    );
    let d = list_attachments(&db, clean_in_folder).await.unwrap()[0].clone();
    assert_eq!(
        d.extract_state,
        ExtractState::Pending,
        "вложение извлекается заново"
    );
    assert_eq!(
        job_of(&db, "attachment", doc.id).await.as_deref(),
        Some("upsert")
    );
    assert_eq!(job_stats(&db).await.unwrap().queued, 4);
    // N1: повторное извлечение не удалось — значения в chunks и FTS всё равно нет
    set_extract_state(&db, doc.id, ExtractState::Failed, Some("parse_error: тест"))
        .await
        .unwrap();
    assert_eq!(
        open_chunks(db.clone()).await,
        0,
        "после неудачного извлечения"
    );
    assert_eq!(
        fts_hits(&db, &pw).await,
        0,
        "FTS после неудачного извлечения"
    );
    let reindex = count(
        &db,
        "SELECT count(*) FROM meta WHERE key = ?1",
        "reindex_pending".into(),
    )
    .await;
    assert_eq!(reindex, 0, "без отметки reindex");
    let (fl0, sig, plain): (String, String, String) = db
        .read(|c| {
            Ok(c.query_row(
                "SELECT (SELECT l0 FROM nodes WHERE kind = 'folder' AND path = 'docs'),
                        (SELECT sig FROM code_symbols WHERE id = 1),
                        (SELECT sig FROM code_symbols WHERE id = 2)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?)
        })
        .await
        .unwrap();
    assert!(!fl0.contains(&pw) && fl0.contains(SECRET_MARK), "{fl0}");
    assert!(!sig.contains(&pw) && sig.contains(SECRET_MARK), "{sig}");
    assert_eq!(plain, "fn plain(x: u32)");
    for flag in [FLAG, FLAG_V3] {
        assert_eq!(meta_value(&db, flag).await, version, "{flag}");
    }
    drain(&db).await;
    // конвейер документов: повторное извлечение из кэша нарезает текст текущими правилами
    replace_attachment_chunks(
        &db,
        doc.id,
        &[Unit {
            locator: Locator::Para(1),
            heading_path: Vec::new(),
            text: format!("token = `{pw}`"),
        }],
    )
    .await
    .unwrap();
    set_extract_state(&db, doc.id, ExtractState::Done, None)
        .await
        .unwrap();
    drain(&db).await;
    assert_eq!(open_chunks(db.clone()).await, 0, "вложение — без значения");
    drop((db, dbs));

    // повторное открытие ничего не делает
    let dbs = UserDbs::new(dd.users_dir());
    let db = dbs.get(user).await.unwrap();
    dbs.wait_rechunk(user).await;
    assert_eq!(job_stats(&db).await.unwrap().queued, 0);
}

/// Повторное ревью 15b, M4: база открыта более новым сервером (флаги `RULES_VERSION + 1`) —
/// откат сервера ничего не перепроверяет и флаги не уменьшает.
#[tokio::test]
async fn newer_rules_flag_is_kept_on_server_rollback() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("secrets-rules-newer");
    let _ = std::fs::remove_dir_all(&dir);
    let dd = DataDir::open(&dir).unwrap();
    let user = Id::new();
    let pw = format!("Sup3r{}", "S3cretValue");
    let db = UserDbs::new(dd.users_dir()).get(user).await.unwrap();
    let k = knowledge(&db).await;
    note_in(&db, k, "ops/db", format!("# База\n\nDB_PASSWORD=`{pw}`\n")).await;
    let newer = (zam_markdown::secrets::RULES_VERSION + 1).to_string();
    let (v, old) = (newer.clone(), pw.clone());
    db.write(move |tx| {
        tx.execute(
            "UPDATE meta SET value = ?1 WHERE key IN (?2, ?3)",
            [v.as_str(), FLAG, FLAG_V3],
        )?;
        // в сохранённом — значение, которое нашли бы текущие правила: перепроверка его бы взяла
        tx.execute(
            "UPDATE chunks SET text = replace(text, ?2, ?1)",
            [old.as_str(), zam_markdown::SECRET_MARK],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    drain(&db).await;
    drop(db);
    let dbs = UserDbs::new(dd.users_dir());
    let db = dbs.get(user).await.unwrap();
    dbs.wait_rechunk(user).await;
    assert_eq!(job_stats(&db).await.unwrap().queued, 0);
    for flag in [FLAG, FLAG_V3] {
        assert_eq!(meta_value(&db, flag).await, newer, "{flag} не уменьшился");
    }
}

/// Повторное ревью 15b, M3: замер разностной перепроверки на базе из 5000 заметок (у каждой
/// десятой — новая утечка). Открытие базы её не ждёт. `cargo test -p zam-store --release --test
/// secrets_rescan -- --ignored delta_rescan_timing --nocapture`.
#[tokio::test]
#[ignore]
async fn delta_rescan_timing() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("secrets-delta-timing");
    let _ = std::fs::remove_dir_all(&dir);
    let dd = DataDir::open(&dir).unwrap();
    let user = Id::new();
    let pw = format!("Sup3r{}", "S3cretValue");
    let db = UserDbs::new(dd.users_dir()).get(user).await.unwrap();
    let k = knowledge(&db).await;
    const N: usize = 5000;
    let body = "Обычный абзац заметки про архитектуру памяти и поиск. ".repeat(20);
    for i in 0..N {
        let extra = if i % 10 == 0 {
            format!("\n\nDB_PASSWORD=`{pw}`\n")
        } else {
            String::new()
        };
        note_in(
            &db,
            k,
            &format!("n/{}/note{i}", i % 50),
            format!("# Заметка {i}\n\n{body}{extra}"),
        )
        .await;
    }
    let old = pw.clone();
    db.write(move |tx| {
        tx.execute(
            "UPDATE meta SET value = '1' WHERE key IN (?1, ?2)",
            [FLAG, FLAG_V3],
        )?;
        tx.execute(
            "UPDATE chunks SET text = replace(text, ?2, ?1)",
            [old.as_str(), zam_markdown::SECRET_MARK],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    drain(&db).await;
    let chunks = count(
        &db,
        "SELECT count(*) FROM chunks WHERE ?1 = ?1",
        String::new(),
    )
    .await;
    drop(db);
    let dbs = UserDbs::new(dd.users_dir());
    let t0 = std::time::Instant::now();
    let db = dbs.get(user).await.unwrap();
    let open = t0.elapsed();
    dbs.wait_rechunk(user).await;
    let total = t0.elapsed();
    let queued = job_stats(&db).await.unwrap().queued;
    println!(
        "заметок {N}, чанков {chunks}: открытие {open:?}, перепроверка до конца {total:?}, заданий {queued}"
    );
    assert_eq!(queued, (N / 10) as i64);
}
