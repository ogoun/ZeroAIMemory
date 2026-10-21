//! Чанки, нарезанные приближённым счётчиком токенов (свежая установка без моделей),
//! перенарезаются в фоне при первом открытии базы с точным счётчиком (§8.2).
//!
//! Отдельный тестовый бинарник: счётчик токенов чанкера — глобальный на процесс.

use std::path::PathBuf;
use std::sync::Arc;

use jiff::Timestamp;
use zam_core::Id;
use zam_markdown::TokenCounter;
use zam_store::{
    Attribution, AuthorKind, DataDir, Db, NewNote, NodePath, SpaceKind, UserDbs, claim_jobs,
    complete_job, create_note, job_stats, list_spaces, set_token_counter,
};

/// «Точный» счётчик теста: число слов (отличается от оценки `символы / 3,5`).
struct Words;

impl TokenCounter for Words {
    fn count(&self, text: &str) -> usize {
        text.split_whitespace().count()
    }
    fn is_exact(&self) -> bool {
        true
    }
}

async fn approx_mark(db: &Db) -> bool {
    db.read(|c| {
        Ok(c.query_row(
            "SELECT EXISTS (SELECT 1 FROM meta WHERE key = 'chunked_approx')",
            [],
            |r| r.get(0),
        )?)
    })
    .await
    .unwrap()
}

/// `(text, token_count)` чанков уровня `chunk`.
async fn leaves(db: &Db, node: Id) -> Vec<(String, i64)> {
    db.read(move |c| {
        let mut st = c.prepare(
            "SELECT text, token_count FROM chunks WHERE node_id = ?1 AND level = 'chunk'
             ORDER BY ordinal",
        )?;
        let rows = st.query_map([node], |r| Ok((r.get(0)?, r.get(1)?)))?;
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

#[tokio::test]
async fn approx_chunks_rechunked_with_exact_counter() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("rechunk-exact");
    let _ = std::fs::remove_dir_all(&dir);
    let dd = DataDir::open(&dir).unwrap();
    let user = Id::new();

    // 1. Приближённый счётчик (по умолчанию): запись ставит отметку в meta.
    let db = UserDbs::new(dd.users_dir()).get(user).await.unwrap();
    assert!(
        !approx_mark(&db).await,
        "новая база без заметок — без отметки"
    );
    let k = list_spaces(&db)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.kind == SpaceKind::Knowledge)
        .unwrap()
        .id;
    let body = "Сборка проекта идёт командой cargo build, тесты отдельно. ".repeat(8);
    let node = create_note(
        &db,
        NewNote {
            space_id: k,
            path: NodePath::parse("сборка").unwrap(),
            content: format!("# Сборка\n\n{body}\n"),
            l0: None,
            attrib: Attribution::new(AuthorKind::Agent),
        },
    )
    .await
    .unwrap()
    .id;
    assert!(approx_mark(&db).await, "чанки по оценке отмечены");
    let before = leaves(&db, node).await;
    assert!(before.iter().all(|(t, n)| *n as usize != Words.count(t)));
    drain(&db).await;
    drop(db);

    // 2. Повторное открытие с приближённым счётчиком — перенарезки нет.
    let dbs = UserDbs::new(dd.users_dir());
    let db = dbs.get(user).await.unwrap();
    dbs.wait_rechunk(user).await;
    assert_eq!(job_stats(&db).await.unwrap().queued, 0);
    drop((db, dbs));

    // 3. Точный счётчик: фоновая перенарезка, задания upsert, отметка снята.
    set_token_counter(Arc::new(Words));
    let dbs = UserDbs::new(dd.users_dir());
    let db = dbs.get(user).await.unwrap();
    dbs.wait_rechunk(user).await;
    let after = leaves(&db, node).await;
    assert!(!after.is_empty());
    for (t, n) in &after {
        assert_eq!(*n as usize, Words.count(t), "токены точным счётчиком");
    }
    assert!(!approx_mark(&db).await, "отметка снята после перенарезки");
    assert_eq!(job_stats(&db).await.unwrap().queued, 1, "upsert заметки");
    drain(&db).await;
    drop((db, dbs));

    // 4. Следующее открытие перенарезку не повторяет.
    let dbs = UserDbs::new(dd.users_dir());
    let db = dbs.get(user).await.unwrap();
    dbs.wait_rechunk(user).await;
    assert_eq!(job_stats(&db).await.unwrap().queued, 0);
}
