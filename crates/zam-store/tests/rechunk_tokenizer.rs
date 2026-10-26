//! Смена токенизатора при том же окне (финальная волна M3): идентификатор токенизатора входит
//! в отпечаток нарезки, база, нарезанная другим точным токенизатором, разово перенарезается
//! при открытии. Без модели (счётчик-оценка) идентификатор не сравнивается — как в M2.
//!
//! Отдельный тестовый бинарник: счётчик токенов чанкера — глобальный на процесс.

use std::path::PathBuf;
use std::sync::Arc;

use jiff::Timestamp;
use zam_core::Id;
use zam_markdown::{ApproxCounter, TokenCounter};
use zam_store::{
    Attribution, AuthorKind, DataDir, Db, NewNote, NodePath, SpaceKind, UserDbs, claim_jobs,
    complete_job, create_note, job_stats, list_spaces, set_token_counter,
};

/// «Точный» счётчик теста: слова, умноженные на `k`; идентификатор — `id`.
struct Scaled {
    k: usize,
    id: &'static str,
}

impl TokenCounter for Scaled {
    fn count(&self, text: &str) -> usize {
        text.split_whitespace().count() * self.k
    }
    fn is_exact(&self) -> bool {
        true
    }
    fn tokenizer_id(&self) -> String {
        self.id.into()
    }
}

/// `token_count` листов узла.
async fn counts(db: &Db, node: Id) -> Vec<i64> {
    db.read(move |c| {
        let mut st = c.prepare(
            "SELECT token_count FROM chunks WHERE node_id = ?1 AND level = 'chunk'
             ORDER BY ordinal",
        )?;
        let rows = st.query_map([node], |r| r.get(0))?;
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

async fn reopen(dd: &DataDir, user: Id) -> Db {
    let dbs = UserDbs::new(dd.users_dir());
    let db = dbs.get(user).await.unwrap();
    dbs.wait_rechunk(user).await;
    db
}

#[tokio::test]
async fn tokenizer_change_rechunks_base() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("rechunk-tokenizer");
    let _ = std::fs::remove_dir_all(&dir);
    let dd = DataDir::open(&dir).unwrap();
    let user = Id::new();

    set_token_counter(Arc::new(Scaled { k: 1, id: "a" }));
    let db = UserDbs::new(dd.users_dir()).get(user).await.unwrap();
    let k = list_spaces(&db)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.kind == SpaceKind::Knowledge)
        .unwrap()
        .id;
    let body = "Сборка проекта идёт командой cargo build, тесты отдельно. ".repeat(4);
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
    let one = counts(&db, node).await;
    assert!(!one.is_empty());
    drain(&db).await;
    drop(db);

    // 1. Тот же токенизатор — без перенарезки.
    let db = reopen(&dd, user).await;
    assert_eq!(job_stats(&db).await.unwrap().queued, 0);
    drop(db);

    // 2. Другой точный токенизатор с тем же окном — разовая перенарезка.
    set_token_counter(Arc::new(Scaled { k: 2, id: "b" }));
    let db = reopen(&dd, user).await;
    let two = counts(&db, node).await;
    assert_eq!(
        two,
        one.iter().map(|n| n * 2).collect::<Vec<_>>(),
        "листы посчитаны новым токенизатором"
    );
    assert_eq!(job_stats(&db).await.unwrap().queued, 1, "upsert заметки");
    drain(&db).await;
    drop(db);
    let db = reopen(&dd, user).await;
    assert_eq!(job_stats(&db).await.unwrap().queued, 0, "один раз");
    drop(db);

    // 3. Модель снята (оценка): идентификатор не сравнивается, база не перенарезается.
    set_token_counter(Arc::new(ApproxCounter));
    let db = reopen(&dd, user).await;
    assert_eq!(counts(&db, node).await, two, "листы прежние");
    assert_eq!(job_stats(&db).await.unwrap().queued, 0);
}
