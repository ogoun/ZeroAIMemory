//! L0 без ревизии (§8.3 ТЗ): экстрактивная сводка заметки от воркера и свёртка папки.

use std::path::PathBuf;

use zam_core::Id;
use zam_store::{
    Attribution, AuthorKind, DataDir, Db, NewNote, NodePath, SpaceKind, UserDbs, claim_jobs,
    complete_job, create_folder, create_note, get_node, list_revisions, list_spaces, set_auto_l0,
    set_folder_l0, update_note,
};

async fn setup(name: &str) -> (Db, Id) {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("l0-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    let dd = DataDir::open(&dir).unwrap();
    let db = UserDbs::new(dd.users_dir()).get(Id::new()).await.unwrap();
    let k = list_spaces(&db)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.kind == SpaceKind::Knowledge)
        .unwrap()
        .id;
    (db, k)
}

async fn note(db: &Db, space: Id, path: &str, content: &str, l0: Option<&str>) -> Id {
    create_note(
        db,
        NewNote {
            space_id: space,
            path: NodePath::parse(path).unwrap(),
            content: content.into(),
            l0: l0.map(str::to_string),
            attrib: Attribution::new(AuthorKind::Agent),
        },
    )
    .await
    .unwrap()
    .id
}

/// Закрывает все задания (чтобы проверить, что запись L0 их не ставит).
async fn drain(db: &Db) {
    for j in claim_jobs(db, 100, jiff::Timestamp::now()).await.unwrap() {
        complete_job(db, j.id, j.claim_gen).await.unwrap();
    }
}

#[tokio::test]
async fn auto_l0_replaced_only_for_auto_source_and_current_rev() {
    let (db, k) = setup("auto").await;
    let n = note(&db, k, "a", "Первый абзац.\n\nВторой абзац важнее.\n", None).await;
    drain(&db).await;
    assert_eq!(
        get_node(&db, n).await.unwrap().l0_source.as_deref(),
        Some("auto")
    );

    assert!(
        set_auto_l0(&db, n, 1, "Второй абзац важнее.")
            .await
            .unwrap()
    );
    let node = get_node(&db, n).await.unwrap();
    assert_eq!(node.l0.as_deref(), Some("Второй абзац важнее."));
    assert_eq!(
        (node.current_rev, node.l0_source.as_deref()),
        (1, Some("auto"))
    );
    // без ревизии и без задания индексации
    assert_eq!(list_revisions(&db, n).await.unwrap().len(), 1);
    assert!(
        claim_jobs(&db, 10, jiff::Timestamp::now())
            .await
            .unwrap()
            .is_empty()
    );
    // то же значение — не изменение
    assert!(
        !set_auto_l0(&db, n, 1, "Второй абзац важнее.")
            .await
            .unwrap()
    );
    // устаревшая ревизия не перезаписывает новую автоматику
    update_note(
        &db,
        n,
        1,
        "Новый текст.\n".into(),
        None,
        Attribution::new(AuthorKind::Agent),
    )
    .await
    .unwrap();
    assert!(!set_auto_l0(&db, n, 1, "старое").await.unwrap());
    assert_eq!(
        get_node(&db, n).await.unwrap().l0.as_deref(),
        Some("Новый текст.")
    );

    // ручной (frontmatter) и агентский L0 автоматика не трогает
    let m = note(&db, k, "m", "---\nl0: Вручную\n---\nТекст.\n", None).await;
    assert!(!set_auto_l0(&db, m, 1, "x").await.unwrap());
    assert_eq!(
        get_node(&db, m).await.unwrap().l0.as_deref(),
        Some("Вручную")
    );
    let g = note(&db, k, "g", "Текст.\n", Some("От агента")).await;
    assert!(!set_auto_l0(&db, g, 1, "x").await.unwrap());
    assert_eq!(
        get_node(&db, g).await.unwrap().l0.as_deref(),
        Some("От агента")
    );
}

#[tokio::test]
async fn folder_l0_written_without_revision() {
    let (db, k) = setup("folder").await;
    let f = create_folder(&db, k, NodePath::parse("dir").unwrap())
        .await
        .unwrap()
        .id;
    let n = note(&db, k, "dir/x", "Текст.\n", None).await;
    assert!(set_folder_l0(&db, f, Some("x: Текст.")).await.unwrap());
    let node = get_node(&db, f).await.unwrap();
    assert_eq!(node.l0.as_deref(), Some("x: Текст."));
    assert_eq!(
        (node.current_rev, node.l0_source.as_deref()),
        (0, Some("auto"))
    );
    assert!(!set_folder_l0(&db, f, Some("x: Текст.")).await.unwrap());
    assert!(set_folder_l0(&db, f, None).await.unwrap());
    assert_eq!(get_node(&db, f).await.unwrap().l0, None);
    // заметка — не папка
    assert!(!set_folder_l0(&db, n, Some("y")).await.unwrap());
    assert_eq!(
        get_node(&db, n).await.unwrap().l0.as_deref(),
        Some("Текст.")
    );
}
