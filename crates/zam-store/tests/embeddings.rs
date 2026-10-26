//! Кэш эмбеддингов (§6.3 ТЗ): dense f16 LE, sparse (u32 id, f16 вес) LE.

use std::path::PathBuf;

use zam_core::Id;
use zam_store::{DataDir, Db, Embedding, UserDbs, get_embeddings, put_embeddings};

async fn setup(name: &str) -> Db {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("emb-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    let dd = DataDir::open(&dir).unwrap();
    UserDbs::new(dd.users_dir()).get(Id::new()).await.unwrap()
}

fn sample(seed: f32) -> Embedding {
    Embedding {
        dense: (0..1024)
            .map(|i| ((i as f32 * 0.37 + seed).sin()) * 0.5)
            .collect(),
        sparse: vec![(0, 0.125), (17, 0.9), (250_001, 0.0031), (u32::MAX, 1.5)],
    }
}

#[tokio::test]
async fn f16_roundtrip_and_sparse() {
    let db = setup("roundtrip").await;
    let (a, b) = (sample(0.0), sample(1.0));
    put_embeddings(
        &db,
        "bge-m3@rev",
        vec![("h1".into(), a.clone()), ("h2".into(), b.clone())],
    )
    .await
    .unwrap();

    let got = get_embeddings(&db, "bge-m3@rev", &["h1".into(), "h2".into(), "нет".into()])
        .await
        .unwrap();
    assert_eq!(got.len(), 2);
    for (h, want) in [("h1", &a), ("h2", &b)] {
        let e = &got[h];
        assert_eq!(e.dense.len(), want.dense.len());
        let max_err = e
            .dense
            .iter()
            .zip(&want.dense)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0f32, f32::max);
        assert!(max_err <= 1e-3, "{h}: {max_err}");
        assert_eq!(e.sparse.len(), want.sparse.len());
        for ((i, w), (wi, ww)) in e.sparse.iter().zip(&want.sparse) {
            assert_eq!(i, wi);
            assert!(
                (w - ww).abs() <= 1e-3 * ww.abs().max(1.0),
                "{i}: {w} vs {ww}"
            );
        }
    }

    // другая модель — другой ключ; повторная запись заменяет
    assert!(
        get_embeddings(&db, "other", &["h1".into()])
            .await
            .unwrap()
            .is_empty()
    );
    let c = Embedding {
        dense: vec![1.0, -2.0],
        sparse: vec![],
    };
    put_embeddings(&db, "bge-m3@rev", vec![("h1".into(), c.clone())])
        .await
        .unwrap();
    let got = get_embeddings(&db, "bge-m3@rev", &["h1".into()])
        .await
        .unwrap();
    assert_eq!(got["h1"], c);
    assert!(
        get_embeddings(&db, "bge-m3@rev", &[])
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn corrupt_blob_is_error() {
    let db = setup("corrupt").await;
    db.write(|tx| {
        tx.execute(
            "INSERT INTO embeddings (text_hash, model_id, dense, sparse, created_at)
             VALUES ('h', 'm', x'0000aa', x'', '2026-10-19T00:00:00.000000Z')",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let err = get_embeddings(&db, "m", &["h".into()]).await.unwrap_err();
    assert!(matches!(err, zam_store::StoreError::Corrupt(_)), "{err}");
}

/// Сборка мусора: строки чанков без ссылки из `chunks.embed_key` и служебные строки
/// удалённых узлов уходят (пачками), живые — остаются; устаревшие служебные строки
/// живого узла снимает `prune_owned_embeddings`.
#[tokio::test]
async fn gc_removes_orphans_and_prune_keeps_current_set() {
    use zam_store::{
        Attribution, AuthorKind, NewNote, NodePath, SpaceKind, create_note, delete_node,
        gc_embeddings, list_spaces, prune_owned_embeddings, put_owned_embeddings,
    };
    let db = setup("gc").await;
    let k = list_spaces(&db)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.kind == SpaceKind::Knowledge)
        .unwrap()
        .id;
    let note = |p: &str| NewNote {
        space_id: k,
        path: NodePath::parse(p).unwrap(),
        content: "# Т\n\nтекст заметки про сборку.\n".into(),
        l0: None,
        attrib: Attribution::new(AuthorKind::Agent),
    };
    let live = create_note(&db, note("live")).await.unwrap().id;
    let gone = create_note(&db, note("gone")).await.unwrap().id;
    let keys = |n: Id| {
        let db = db.clone();
        async move {
            db.read(move |c| {
                let mut st = c.prepare("SELECT embed_key FROM chunks WHERE node_id = ?1")?;
                let rows = st.query_map([n], |r| r.get::<_, String>(0))?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap()
        }
    };
    let live_keys = keys(live).await;
    let gone_keys = keys(gone).await;
    let m = "m@1";
    let mut rows: Vec<(String, Embedding)> = live_keys
        .iter()
        .chain(&gone_keys)
        .map(|k| (k.clone(), sample(1.0)))
        .collect();
    // сироты без ссылок — больше одной пачки
    rows.extend((0..7).map(|i| (format!("orphan{i}"), sample(2.0))));
    put_embeddings(&db, m, rows.clone()).await.unwrap();
    put_embeddings(&db, "other@2", rows).await.unwrap();
    put_owned_embeddings(
        &db,
        m,
        live,
        vec![("l0a".into(), sample(3.0)), ("l0b".into(), sample(3.0))],
    )
    .await
    .unwrap();
    put_owned_embeddings(&db, m, gone, vec![("l0g".into(), sample(3.0))])
        .await
        .unwrap();
    delete_node(&db, gone, zam_store::AuthorKind::System)
        .await
        .unwrap();

    let count = || {
        let db = db.clone();
        async move {
            db.read(|c| {
                Ok(c.query_row("SELECT count(*) FROM embeddings", [], |r| {
                    r.get::<_, i64>(0)
                })?)
            })
            .await
            .unwrap()
        }
    };
    let total = count().await;
    // пачками по 4: сироты обеих моделей, ключи удалённой заметки, служебная строка удалённой
    let mut deleted = 0;
    loop {
        let n = gc_embeddings(&db, 4).await.unwrap();
        assert!(n <= 4);
        deleted += n;
        if n < 4 {
            break;
        }
    }
    let want = 2 * (7 + gone_keys.len() as u64) + 1;
    assert_eq!(deleted, want);
    assert_eq!(count().await, total - want as i64);
    let left = get_embeddings(
        &db,
        m,
        &[
            live_keys.clone(),
            vec!["l0a".into(), "l0b".into(), "l0g".into(), "orphan0".into()],
        ]
        .concat(),
    )
    .await
    .unwrap();
    assert_eq!(left.len(), live_keys.len() + 2);
    assert!(!left.contains_key("l0g") && !left.contains_key("orphan0"));
    assert_eq!(gc_embeddings(&db, 4).await.unwrap(), 0);

    // текущий набор L0 узла — только l0b
    assert_eq!(
        prune_owned_embeddings(&db, m, live, &["l0b".into()])
            .await
            .unwrap(),
        1
    );
    let l0 = get_embeddings(&db, m, &["l0a".into(), "l0b".into()])
        .await
        .unwrap();
    assert_eq!(l0.keys().collect::<Vec<_>>(), vec!["l0b"]);

    // база с чанком без ключа (до v5) — сборка мусора ничего не трогает
    db.write(|tx| {
        tx.execute(
            "UPDATE chunks SET embed_key = NULL WHERE rowid = (SELECT min(rowid) FROM chunks)",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    put_embeddings(&db, m, vec![("orphan-late".into(), sample(4.0))])
        .await
        .unwrap();
    assert_eq!(gc_embeddings(&db, 100).await.unwrap(), 0);
}

/// Служебная строка до v5 (без владельца) после `adopt_owned_embeddings` принадлежит узлу
/// и сборкой мусора не снимается; исключённые из пересборки базы — в `system.db`.
#[tokio::test]
async fn adopt_legacy_owner_and_index_skipped() {
    use zam_store::{
        Attribution, AuthorKind, NewNote, NodePath, SpaceKind, SystemStore, adopt_owned_embeddings,
        adopt_owned_embeddings_batch, create_note, gc_embeddings, list_spaces,
    };
    let db = setup("adopt").await;
    let k = list_spaces(&db)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.kind == SpaceKind::Knowledge)
        .unwrap()
        .id;
    let n = create_note(
        &db,
        NewNote {
            space_id: k,
            path: NodePath::parse("f/n").unwrap(),
            content: "текст".into(),
            l0: None,
            attrib: Attribution::new(AuthorKind::Agent),
        },
    )
    .await
    .unwrap()
    .id;
    put_embeddings(&db, "m@1", vec![("legacy".into(), sample(1.0))])
        .await
        .unwrap();
    assert_eq!(
        adopt_owned_embeddings(&db, "m@1", n, &["legacy".into(), "нет".into()])
            .await
            .unwrap(),
        1
    );
    // пачкой (reindex из кэша: строки папок одной транзакцией)
    put_embeddings(
        &db,
        "m@1",
        vec![("l2".into(), sample(2.0)), ("l3".into(), sample(3.0))],
    )
    .await
    .unwrap();
    assert_eq!(
        adopt_owned_embeddings_batch(
            &db,
            "m@1",
            vec![(n, "l2".into()), (n, "l3".into()), (n, "нет".into())]
        )
        .await
        .unwrap(),
        2
    );
    assert_eq!(
        adopt_owned_embeddings_batch(&db, "m@1", vec![])
            .await
            .unwrap(),
        0
    );
    gc_embeddings(&db, 100).await.unwrap();
    assert_eq!(
        get_embeddings(&db, "m@1", &["legacy".into(), "l2".into(), "l3".into()])
            .await
            .unwrap()
            .len(),
        3
    );

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("emb-skip");
    let _ = std::fs::remove_dir_all(&dir);
    let dd = DataDir::open(&dir).unwrap();
    let sys = SystemStore::open(&dd).unwrap();
    let u = sys.create_user("u", false).await.unwrap().id;
    assert!(sys.index_skipped().await.unwrap().is_empty());
    sys.index_skip(u, "файл не SQLite", 1).await.unwrap();
    sys.index_skip(u, "файл не SQLite", 5).await.unwrap();
    let s = sys.index_skipped().await.unwrap();
    assert_eq!((s.len(), s[0].user_id, s[0].attempts), (1, u, 5));
    // повторная отметка reindex (attempts = 1) счётчик попыток воркера не затирает
    sys.index_skip(u, "файл всё ещё не SQLite", 1)
        .await
        .unwrap();
    let s = sys.index_skipped().await.unwrap();
    assert_eq!(
        (s[0].attempts, s[0].error.as_str()),
        (5, "файл всё ещё не SQLite")
    );
    sys.index_unskip(u).await.unwrap();
    assert!(sys.index_skipped().await.unwrap().is_empty());
}
