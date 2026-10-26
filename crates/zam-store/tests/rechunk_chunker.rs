//! Смена алгоритма или настроек нарезки (задача 17 M3): база, нарезанная до задачи 17 или
//! с другими `[index] chunk_*`, разово перенарезается в фоне при открытии; листы, текст и
//! префикс которых не изменились, сохраняют `embed_key` (вектор берётся из кэша). L0 в
//! префиксе листов — по `set_prefix_l0`. Заметка и запись, нарезка которых не изменилась, не
//! переписываются и не получают `upsert`; флаг L0 записи журналов не перенарезает (финальная
//! волна M3).
//!
//! Отдельный тестовый бинарник: пороги чанкера и флаг L0 — глобальные на процесс.

use std::path::PathBuf;

use jiff::Timestamp;
use zam_core::Id;
use zam_markdown::ChunkParams;
use zam_store::{
    Attribution, AuthorKind, DataDir, Db, EntryKind, NewEntry, NewNote, NodePath, SpaceKind,
    UserDbs, claim_jobs, complete_job, create_entry, create_note, job_stats, list_spaces,
    set_chunk_params, set_prefix_l0,
};

/// `(heading_path, context_prefix, embed_key)` листов узла.
async fn leaves(db: &Db, node: Id) -> Vec<(String, String, String)> {
    db.read(move |c| {
        let mut st = c.prepare(
            "SELECT heading_path, context_prefix, embed_key FROM chunks
             WHERE node_id = ?1 AND level = 'chunk' ORDER BY ordinal",
        )?;
        let rows = st.query_map([node], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    })
    .await
    .unwrap()
}

/// `(rowid, embed_key)` чанков владельца (`col` — `node_id` или `entry_id`): переписанные
/// чанки получают новые `rowid`.
async fn rows(db: &Db, col: &'static str, id: Id) -> Vec<(i64, String)> {
    db.read(move |c| {
        let mut st = c.prepare(&format!(
            "SELECT rowid, embed_key FROM chunks WHERE {col} = ?1 ORDER BY ordinal"
        ))?;
        let rows = st.query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?;
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

async fn note(db: &Db, space: Id, path: &str, content: String) -> Id {
    create_note(
        db,
        NewNote {
            space_id: space,
            path: NodePath::parse(path).unwrap(),
            content,
            l0: Some("Сводка заметки для префикса".into()),
            attrib: Attribution::new(AuthorKind::Agent),
        },
    )
    .await
    .unwrap()
    .id
}

async fn reopen(dd: &DataDir, user: Id) -> Db {
    let dbs = UserDbs::new(dd.users_dir());
    let db = dbs.get(user).await.unwrap();
    dbs.wait_rechunk(user).await;
    db
}

#[tokio::test]
async fn base_rechunked_once_when_chunker_changes() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("rechunk-chunker");
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
    let para = "Перенос данных идёт пакетами, каждый пакет проверяется. ".repeat(30);
    let deep = note(
        &db,
        k,
        "глубокая",
        format!("# Перенос\n\n{para}\n\n#### Проверка\n\n{para}\n"),
    )
    .await;
    let flat = note(&db, k, "плоская", format!("# Плоская\n\n{para}\n")).await;
    let (entry, _) = create_entry(
        &db,
        NewEntry {
            space_id: k,
            kind: EntryKind::Insight,
            title: "Пакеты переноса проверяются".into(),
            body: "Каждый пакет сверяется по контрольной сумме.".into(),
            data: serde_json::json!({"kind": "gotcha"}),
            area: None,
            tags: Vec::new(),
            status: None,
            certainty: None,
            idempotency_key: None,
            triggers: Vec::new(),
            links: Vec::new(),
        },
        &Attribution::new(AuthorKind::Agent),
    )
    .await
    .unwrap();
    let entry = entry.id;
    drain(&db).await;
    let deep_now = leaves(&db, deep).await;
    assert!(
        deep_now.iter().any(|(h, _, _)| h == "Перенос > Проверка"),
        "H4 в пути: {deep_now:?}"
    );
    let flat_now = leaves(&db, flat).await;
    let flat_rows = rows(&db, "node_id", flat).await;
    let entry_rows = rows(&db, "entry_id", entry).await;

    // 1. База «до задачи 17»: отпечатка нет, путь листов — без H4.
    exec(
        &db,
        "DELETE FROM meta WHERE key = 'chunker';
         UPDATE chunks SET heading_path = 'Перенос', embed_key = 'старый'
         WHERE heading_path = 'Перенос > Проверка';",
    )
    .await;
    drop(db);
    let db = reopen(&dd, user).await;
    let after = leaves(&db, deep).await;
    assert_eq!(after, deep_now, "перенарезано текущим алгоритмом");
    assert_eq!(
        leaves(&db, flat).await,
        flat_now,
        "неизменённые листы — тот же embed_key"
    );
    assert_eq!(
        rows(&db, "node_id", flat).await,
        flat_rows,
        "неизменённая заметка не переписана"
    );
    assert_eq!(rows(&db, "entry_id", entry).await, entry_rows);
    assert_eq!(
        job_stats(&db).await.unwrap().queued,
        1,
        "upsert только изменённой заметки"
    );
    drain(&db).await;
    drop(db);

    // 2. Повторное открытие — без перенарезки.
    let db = reopen(&dd, user).await;
    assert_eq!(job_stats(&db).await.unwrap().queued, 0);
    drop(db);

    // 3. Другие настройки нарезки (λ и L0 в префиксе) — снова разовая перенарезка.
    set_chunk_params(ChunkParams {
        boundary_lambda: 1.0,
        ..ChunkParams::default()
    });
    set_prefix_l0(true);
    let db = reopen(&dd, user).await;
    assert_eq!(
        job_stats(&db).await.unwrap().queued,
        2,
        "обе заметки (префикс с L0), запись не изменилась"
    );
    for (_, prefix, _) in leaves(&db, flat).await {
        assert!(
            prefix.ends_with(" — Сводка заметки для префикса"),
            "{prefix:?}"
        );
    }
    assert_eq!(rows(&db, "entry_id", entry).await, entry_rows);
    drain(&db).await;
    drop(db);
    let db = reopen(&dd, user).await;
    assert_eq!(job_stats(&db).await.unwrap().queued, 0);

    // 4. Только флаг L0: записи журналов не перенарезаются (испорченный ключ остаётся).
    exec(
        &db,
        "UPDATE chunks SET embed_key = 'старый' WHERE entry_id IS NOT NULL",
    )
    .await;
    drop(db);
    set_prefix_l0(false);
    let db = reopen(&dd, user).await;
    assert_eq!(job_stats(&db).await.unwrap().queued, 2, "только заметки");
    assert!(
        rows(&db, "entry_id", entry)
            .await
            .iter()
            .all(|(_, k)| k == "старый"),
        "запись не перенарезана"
    );
}
