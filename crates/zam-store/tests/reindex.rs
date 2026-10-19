//! Опоры пересборки индекса из кэша (задача 10 M2): ключ кэша в `chunks.embed_key`
//! (миграция 0005 и дозаполнение), страницы индексируемых заметок, задания промахов,
//! отметка незавершённого reindex, папки с L0, неудавшиеся задания.

use std::path::PathBuf;

use jiff::Timestamp;
use zam_core::Id;
use zam_store::{
    Attribution, AuthorKind, DataDir, Db, NewNote, NodePath, SpaceKind, UserDbs, chunks_for_index,
    claim_jobs, complete_job, create_folder, create_note, embed_key_of, enqueue_upserts, fail_job,
    failed_jobs, finish_reindexed, folders_for_index, get_node, indexable_count, list_spaces,
    mark_reindex, move_node, pending_reindex, reindex_page, set_folder_l0,
};

fn dir(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("reidx-{name}"));
    let _ = std::fs::remove_dir_all(&d);
    d
}

async fn spaces(db: &Db) -> (Id, Id, Id) {
    let s = list_spaces(db).await.unwrap();
    let find = |k: SpaceKind| s.iter().find(|x| x.kind == k).unwrap().id;
    (
        find(SpaceKind::Knowledge),
        find(SpaceKind::Rules),
        find(SpaceKind::Human),
    )
}

async fn add(db: &Db, space: Id, path: &str, content: &str) -> Id {
    create_note(
        db,
        NewNote {
            space_id: space,
            path: NodePath::parse(path).unwrap(),
            content: content.into(),
            l0: None,
            attrib: Attribution::new(AuthorKind::Agent),
        },
    )
    .await
    .unwrap()
    .id
}

/// `(context_prefix, text, embed_key)` всех чанков узла прямо из таблицы.
async fn raw_keys(db: &Db, node: Id) -> Vec<(String, String, Option<String>)> {
    db.read(move |c| {
        let mut st = c.prepare(
            "SELECT context_prefix, text, embed_key FROM chunks WHERE node_id = ?1 ORDER BY ordinal",
        )?;
        let rows = st.query_map([node], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    })
    .await
    .unwrap()
}

fn assert_keys(rows: &[(String, String, Option<String>)]) {
    assert!(!rows.is_empty());
    for (prefix, text, key) in rows {
        let want = embed_key_of(prefix, text);
        assert_eq!(key.as_deref(), Some(want.as_str()), "{prefix}");
        // формула — blake3 строки `префикс + "\n" + текст`
        assert_eq!(
            want,
            blake3::hash(format!("{prefix}\n{text}").as_bytes())
                .to_hex()
                .to_string()
        );
    }
}

const NOTE: &str =
    "# Сборка\n\nСобираем проект командой cargo build.\n\n## Детали\n\nПеременные окружения.\n";

/// Ключ пишется вместе с чанком, пересчитывается при переносе (меняется префикс),
/// строки без ключа (база до v5) дозаполняются при открытии.
#[tokio::test]
async fn embed_key_written_moved_and_backfilled() {
    let d = dir("key");
    let dd = DataDir::open(&d).unwrap();
    let user = Id::new();
    let (n, before) = {
        let users = UserDbs::new(dd.users_dir());
        let db = users.get(user).await.unwrap();
        let (k, ..) = spaces(&db).await;
        let n = add(&db, k, "a/note", NOTE).await;
        assert_keys(&raw_keys(&db, n).await);
        let before = raw_keys(&db, n).await;
        move_node(&db, n, NodePath::parse("b/renamed").unwrap())
            .await
            .unwrap();
        let after = raw_keys(&db, n).await;
        assert_keys(&after);
        assert_ne!(before[0].2, after[0].2, "префикс сменился — ключ тоже");
        // чанки для воркера несут тот же ключ
        let ic = chunks_for_index(&db, n).await.unwrap();
        assert!(
            ic.iter()
                .all(|c| c.embed_key == embed_key_of(&c.context_prefix, &c.text))
        );
        // база «до v5»: ключи стёрты
        db.write(|tx| {
            tx.execute("UPDATE chunks SET embed_key = NULL", [])?;
            Ok(())
        })
        .await
        .unwrap();
        (n, after)
    };
    // новое открытие базы дозаполняет ключи
    let users = UserDbs::new(dd.users_dir());
    let db = users.get(user).await.unwrap();
    let rows = raw_keys(&db, n).await;
    assert_keys(&rows);
    assert_eq!(rows, before);
    let ver: u32 = db
        .read(|c| Ok(c.query_row("PRAGMA user_version", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(ver, 5);
}

/// Страницы индексируемых заметок (без Human, `index: false`, удалённых), задания для
/// промахов кэша, отметка восстановленных из кэша, неудавшиеся задания.
#[tokio::test]
async fn pages_misses_and_finish() {
    let dd = DataDir::open(&dir("pages")).unwrap();
    let db = UserDbs::new(dd.users_dir()).get(Id::new()).await.unwrap();
    let (k, r, h) = spaces(&db).await;
    let mut notes = Vec::new();
    for i in 0..5 {
        notes.push(add(&db, k, &format!("n{i}"), NOTE).await);
    }
    let rule = add(&db, r, "rule", NOTE).await;
    add(&db, h, "human", NOTE).await;
    add(&db, k, "off", &format!("---\nindex: false\n---\n{NOTE}")).await;
    assert_eq!(indexable_count(&db, None).await.unwrap(), 6);
    assert_eq!(indexable_count(&db, Some(r)).await.unwrap(), 1);
    // страницы по 2 заметки по возрастанию id
    let mut seen = Vec::new();
    let mut after = None;
    loop {
        let page = reindex_page(&db, Some(k), after, 2).await.unwrap();
        let Some(last) = page.last() else { break };
        assert!(page.len() <= 2);
        after = Some(last.node_id);
        for p in &page {
            assert_eq!(p.rev, 1);
            assert!(!p.chunks.is_empty());
        }
        seen.extend(page.into_iter().map(|p| p.node_id));
    }
    let mut want = notes.clone();
    want.sort();
    assert_eq!(seen, want);
    assert_eq!(
        reindex_page(&db, Some(r), None, 10).await.unwrap()[0].node_id,
        rule
    );

    // все задания выполнены (как после индексации), одно — failed после 5 неудач
    let mut t = Timestamp::now();
    let jobs = claim_jobs(&db, 100, t).await.unwrap();
    let mut j0 = None;
    for j in jobs {
        if j.target_id == notes[0] {
            j0 = Some(j);
        } else {
            complete_job(&db, j.id, j.claim_gen).await.unwrap();
        }
    }
    let mut j = j0.unwrap();
    for i in 0..5 {
        fail_job(&db, j.id, j.claim_gen, "Qdrant: отказ", t)
            .await
            .unwrap();
        if i < 4 {
            t += jiff::SignedDuration::from_hours(2);
            j = claim_jobs(&db, 100, t).await.unwrap().remove(0);
        }
    }
    let failed = failed_jobs(&db, 20).await.unwrap();
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert_eq!(failed[0].node_id, notes[0]);
    assert_eq!(failed[0].error.as_deref(), Some("Qdrant: отказ"));
    assert_eq!(get_node(&db, notes[0]).await.unwrap().index_state, "failed");

    // восстановлены из кэша: failed снят, узел indexed
    finish_reindexed(&db, vec![(notes[0], 1), (notes[1], 1)])
        .await
        .unwrap();
    assert!(failed_jobs(&db, 20).await.unwrap().is_empty());
    assert_eq!(
        get_node(&db, notes[0]).await.unwrap().index_state,
        "indexed"
    );
    // промах кэша: задание и pending; неизвестный узел — без задания
    let n = enqueue_upserts(&db, vec![notes[2], Id::new()])
        .await
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(
        get_node(&db, notes[2]).await.unwrap().index_state,
        "pending"
    );
    // узел с активным заданием finish_reindexed не отмечает
    finish_reindexed(&db, vec![(notes[2], 1)]).await.unwrap();
    assert_eq!(
        get_node(&db, notes[2]).await.unwrap().index_state,
        "pending"
    );
}

/// Отметка незавершённого reindex и папки с L0 (кроме Human).
#[tokio::test]
async fn reindex_mark_and_folders() {
    let dd = DataDir::open(&dir("mark")).unwrap();
    let db = UserDbs::new(dd.users_dir()).get(Id::new()).await.unwrap();
    let (k, _, h) = spaces(&db).await;
    assert_eq!(pending_reindex(&db).await.unwrap(), None);
    mark_reindex(&db, Some("{\"full\":false}".into()))
        .await
        .unwrap();
    assert_eq!(
        pending_reindex(&db).await.unwrap().as_deref(),
        Some("{\"full\":false}")
    );
    mark_reindex(&db, None).await.unwrap();
    assert_eq!(pending_reindex(&db).await.unwrap(), None);

    let f = create_folder(&db, k, NodePath::parse("dir").unwrap())
        .await
        .unwrap();
    let hf = create_folder(&db, h, NodePath::parse("hdir").unwrap())
        .await
        .unwrap();
    create_folder(&db, k, NodePath::parse("empty").unwrap())
        .await
        .unwrap();
    set_folder_l0(&db, f.id, Some("note: про сборку"))
        .await
        .unwrap();
    set_folder_l0(&db, hf.id, Some("личное")).await.unwrap();
    let fs = folders_for_index(&db, None).await.unwrap();
    assert_eq!(fs.len(), 1);
    assert_eq!(
        (fs[0].node_id, fs[0].l0.as_str()),
        (f.id, "note: про сборку")
    );
    assert_eq!(fs[0].space_name, "Knowledge");
}
