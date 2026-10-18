use std::path::PathBuf;

use zam_core::Id;
use zam_store::{
    Attribution, AuthorKind, DataDir, Db, NewNote, NodeKind, NodePath, SpaceKind, StoreError,
    USER_MIGRATIONS, UserDbs, create_folder, create_note, find_by_name, get_node, get_node_by_path,
    list_children, list_spaces, node_content,
};

fn datadir(name: &str) -> DataDir {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("nodes-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    DataDir::open(&dir).unwrap()
}

/// База пользователя и id пространства Knowledge.
async fn setup(name: &str) -> (Db, Id) {
    let dd = datadir(name);
    let db = UserDbs::new(dd.users_dir()).get(Id::new()).await.unwrap();
    let space = list_spaces(&db)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.kind == SpaceKind::Knowledge)
        .unwrap()
        .id;
    (db, space)
}

fn p(s: &str) -> NodePath {
    NodePath::parse(s).unwrap()
}

fn agent() -> Attribution {
    Attribution {
        author_kind: AuthorKind::Agent,
        token_id: Some(Id::new()),
        session_id: Some("s1".into()),
        machine: Some("box".into()),
        message: Some("первая версия".into()),
    }
}

fn note(space: Id, path: &str, content: &str) -> NewNote {
    NewNote {
        space_id: space,
        path: p(path),
        content: content.into(),
        l0: None,
        attrib: agent(),
    }
}

async fn count(db: &Db, sql: &'static str, arg: String) -> i64 {
    db.read(move |c| Ok(c.query_row(sql, [arg], |r| r.get(0))?))
        .await
        .unwrap()
}

async fn chunk_count(db: &Db, node: Id) -> i64 {
    count(
        db,
        "SELECT count(*) FROM chunks WHERE node_id = ?1",
        node.to_string(),
    )
    .await
}

// ---------- NodePath ----------

#[test]
fn node_path_accepts_valid() {
    for s in ["Инструменты/Bash скрипты", "a", "a/b.c"] {
        assert_eq!(NodePath::parse(s).unwrap().as_str(), s);
    }
    let np = p("Инструменты/Bash скрипты");
    assert_eq!(np.name(), "Bash скрипты");
    assert_eq!(np.parent().unwrap().as_str(), "Инструменты");
    assert!(np.parent().unwrap().parent().is_none());
    assert_eq!(p("a").name(), "a");
}

#[test]
fn node_path_rejects_invalid() {
    let long = "я".repeat(128); // 256 байт
    let cases = [
        "",
        "/a",
        "a/",
        "a//b",
        "a/../b",
        ".",
        "a\\b",
        "a\u{0007}",
        long.as_str(),
    ];
    for s in cases {
        assert!(
            matches!(NodePath::parse(s), Err(StoreError::BadInput(_))),
            "должен быть отклонён: {s:?}"
        );
    }
    // граница: сегмент ровно 255 байт допустим, путь > 1024 байт — нет
    assert!(NodePath::parse(&"a".repeat(255)).is_ok());
    let seg = "a".repeat(200);
    let too_long = [seg.as_str(); 6].join("/");
    assert!(matches!(
        NodePath::parse(&too_long),
        Err(StoreError::BadInput(_))
    ));
}

// ---------- создание и чтение ----------

#[tokio::test]
async fn create_note_makes_parent_folder() {
    let (db, space) = setup("create").await;
    let n = create_note(
        &db,
        note(space, "Инструменты/Bash скрипты", "# Bash\n\nтекст\n"),
    )
    .await
    .unwrap();
    assert_eq!(n.kind, NodeKind::Note);
    assert_eq!(n.current_rev, 1);
    assert_eq!(n.name, "Bash скрипты");
    assert_eq!(n.path, "Инструменты/Bash скрипты");
    assert_eq!(n.space_id, space);
    assert!(n.content_hash.as_deref().is_some_and(|h| h.len() == 64));
    assert!(n.deleted_at.is_none() && !n.has_children);

    let folder = get_node_by_path(&db, space, &p("Инструменты"))
        .await
        .unwrap();
    assert_eq!(folder.kind, NodeKind::Folder);
    assert_eq!(folder.current_rev, 0);
    assert!(folder.parent_id.is_none());
    assert_eq!(n.parent_id, Some(folder.id));

    let found = get_node_by_path(&db, space, &p("Инструменты/Bash скрипты"))
        .await
        .unwrap();
    assert_eq!(found, n);
    assert_eq!(get_node(&db, n.id).await.unwrap(), n);

    let (rev, text) = node_content(&db, n.id, None).await.unwrap();
    assert_eq!((rev, text.as_str()), (1, "# Bash\n\nтекст\n"));
    assert_eq!(node_content(&db, n.id, Some(1)).await.unwrap().0, 1);
    assert!(matches!(
        node_content(&db, n.id, Some(2)).await,
        Err(StoreError::NotFound)
    ));
    assert!(matches!(
        get_node(&db, Id::new()).await,
        Err(StoreError::NotFound)
    ));
    assert!(matches!(
        get_node_by_path(&db, space, &p("нет/такого")).await,
        Err(StoreError::NotFound)
    ));

    // ревизия хранит атрибуцию
    let (author, msg, size): (String, String, i64) = db
        .read(move |c| {
            Ok(c.query_row(
                "SELECT author_kind, message, size FROM revisions WHERE node_id = ?1 AND rev = 1",
                [n.id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(author, "agent");
    assert_eq!(msg, "первая версия");
    assert_eq!(size, "# Bash\n\nтекст\n".len() as i64);
}

#[tokio::test]
async fn create_conflicts() {
    let (db, space) = setup("conflict").await;
    create_note(&db, note(space, "a/b", "x")).await.unwrap();
    assert!(matches!(
        create_note(&db, note(space, "a/b", "y")).await,
        Err(StoreError::Conflict(_))
    ));
    // предок — заметка
    assert!(matches!(
        create_note(&db, note(space, "a/b/c", "y")).await,
        Err(StoreError::Conflict(_))
    ));
    // папка поверх существующей папки
    assert!(matches!(
        create_folder(&db, space, p("a")).await,
        Err(StoreError::Conflict(_))
    ));
    // неизвестное пространство
    assert!(matches!(
        create_note(&db, note(Id::new(), "z", "y")).await,
        Err(StoreError::NotFound)
    ));
    let f = create_folder(&db, space, p("a/d/e")).await.unwrap();
    assert_eq!(f.kind, NodeKind::Folder);
    assert_eq!(f.current_rev, 0);
    assert!(f.content_hash.is_none());
    assert!(matches!(
        node_content(&db, f.id, None).await,
        Err(StoreError::NotFound)
    ));
}

#[tokio::test]
async fn list_children_and_find_by_name() {
    let (db, space) = setup("list").await;
    create_note(&db, note(space, "Инструменты/Bash скрипты", "x"))
        .await
        .unwrap();
    create_note(&db, note(space, "Инструменты/Под/Глубоко", "x"))
        .await
        .unwrap();
    create_note(&db, note(space, "Альфа", "x")).await.unwrap();
    create_note(&db, note(space, "Под/Глубоко", "x"))
        .await
        .unwrap();

    let top = list_children(&db, space, None, 1).await.unwrap();
    let names: Vec<_> = top.iter().map(|n| (n.kind, n.name.as_str())).collect();
    assert_eq!(
        names,
        vec![
            (NodeKind::Folder, "Инструменты"),
            (NodeKind::Folder, "Под"),
            (NodeKind::Note, "Альфа"),
        ]
    );
    assert!(top[0].has_children);
    assert!(!top[2].has_children);

    let folder = top[0].id;
    let sub = list_children(&db, space, Some(folder), 1).await.unwrap();
    let names: Vec<_> = sub.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(names, vec!["Под", "Bash скрипты"]);

    // depth 2: плоский pre-order
    let deep = list_children(&db, space, Some(folder), 2).await.unwrap();
    let paths: Vec<_> = deep.iter().map(|n| n.path.as_str()).collect();
    assert_eq!(
        paths,
        vec![
            "Инструменты/Под",
            "Инструменты/Под/Глубоко",
            "Инструменты/Bash скрипты"
        ]
    );
    assert_eq!(deep[1].parent_id, Some(deep[0].id));

    let all = list_children(&db, space, None, 10).await.unwrap();
    assert_eq!(all.len(), 7);

    let found = find_by_name(&db, space, "Глубоко").await.unwrap();
    let paths: Vec<_> = found.iter().map(|n| n.path.as_str()).collect();
    assert_eq!(paths, vec!["Инструменты/Под/Глубоко", "Под/Глубоко"]);
    // папки не находятся
    assert!(find_by_name(&db, space, "Под").await.unwrap().is_empty());
}

// ---------- чанки и FTS ----------

const THREE: &str = "# Обзор\n\nОбщие сведения.\n\n## Установка\n\nСтавим скрипты в PATH.\n\n#### Детали\n\nвнутри секции\n\n## Использование\n\nЗапуск через core.autocrlf.\n";

#[tokio::test]
async fn chunks_per_section_and_fts() {
    let (db, space) = setup("chunks").await;
    let n = create_note(&db, note(space, "Инструменты/Bash скрипты", THREE))
        .await
        .unwrap();
    assert_eq!(chunk_count(&db, n.id).await, 3);

    let rows: Vec<(String, i64, String, String, String, String)> = db
        .read(move |c| {
            let mut st = c.prepare(
                "SELECT heading_path, ordinal, text, context_prefix, level, text_hash
                 FROM chunks WHERE node_id = ?1 ORDER BY ordinal",
            )?;
            let rows = st.query_map([n.id], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
        .unwrap();
    let heads: Vec<_> = rows.iter().map(|r| r.0.as_str()).collect();
    assert_eq!(
        heads,
        vec!["Обзор", "Обзор > Установка", "Обзор > Использование"]
    );
    assert_eq!(rows.iter().map(|r| r.1).collect::<Vec<_>>(), vec![0, 1, 2]);
    assert!(!rows[1].2.contains("## Установка"));
    assert!(rows[1].2.contains("Ставим скрипты") && rows[1].2.contains("#### Детали"));
    assert_eq!(
        rows[1].3,
        "[Knowledge/Инструменты/Bash скрипты] Bash скрипты > Обзор > Установка"
    );
    assert!(rows.iter().all(|r| r.4 == "section" && r.5.len() == 64));

    let hits = count(
        &db,
        "SELECT count(*) FROM chunks_fts WHERE chunks_fts MATCH ?1",
        "скрипты".into(),
    )
    .await;
    assert_eq!(hits, 1);
    let tri = count(
        &db,
        "SELECT count(*) FROM chunks_fts_tri WHERE chunks_fts_tri MATCH ?1",
        "\"core.autocrlf\"".into(),
    )
    .await;
    assert_eq!(tri, 1);
    let by_heading = count(
        &db,
        "SELECT count(*) FROM chunks_fts WHERE chunks_fts MATCH ?1",
        "heading_path : использование".into(),
    )
    .await;
    assert_eq!(by_heading, 1);
}

#[tokio::test]
async fn preamble_chunk_and_whitespace_sections() {
    let (db, space) = setup("preamble").await;
    let src =
        "---\ntags: [a]\n---\nВступление до заголовков.\n\n# Пустая\n\n   \n# Полная\n\nтекст\n";
    let n = create_note(&db, note(space, "Пре", src)).await.unwrap();
    let rows: Vec<(String, String, String)> = db
        .read(move |c| {
            let mut st = c.prepare(
                "SELECT heading_path, text, context_prefix FROM chunks
                 WHERE node_id = ?1 ORDER BY ordinal",
            )?;
            let rows = st.query_map([n.id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
        .unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0].0, "");
    assert!(rows[0].1.starts_with("Вступление") && !rows[0].1.contains("tags"));
    assert_eq!(rows[0].2, "[Knowledge/Пре] Пре");
    assert_eq!(rows[1].0, "Полная");
    assert_eq!(
        n.frontmatter,
        Some(serde_json::json!({"tags": ["a"]})),
        "null-поля опускаются"
    );
}

#[tokio::test]
async fn index_false_no_chunks() {
    let (db, space) = setup("noindex").await;
    let src = "---\nindex: false\n---\n# Секрет\n\nскрипты\n";
    let n = create_note(&db, note(space, "x", src)).await.unwrap();
    assert_eq!(chunk_count(&db, n.id).await, 0);
    assert_eq!(n.frontmatter.unwrap()["index"], serde_json::json!(false));
    let all = count(
        &db,
        "SELECT count(*) FROM chunks_fts WHERE chunks_fts MATCH ?1",
        "скрипты".into(),
    )
    .await;
    assert_eq!(all, 0);
}

// ---------- L0 ----------

#[tokio::test]
async fn l0_priority() {
    let (db, space) = setup("l0").await;
    let fm_full = "---\nl0: Из frontmatter\ndescription: Описание\n---\nПервый абзац.\n";

    let mut a = note(space, "agent", fm_full);
    a.l0 = Some("От агента".into());
    let n = create_note(&db, a).await.unwrap();
    assert_eq!(n.l0.as_deref(), Some("От агента"));
    assert_eq!(n.l0_source.as_deref(), Some("agent"));

    let n = create_note(&db, note(space, "manual", fm_full))
        .await
        .unwrap();
    assert_eq!(n.l0.as_deref(), Some("Из frontmatter"));
    assert_eq!(n.l0_source.as_deref(), Some("manual"));

    let src = "---\ndescription: Описание\n---\nПервый абзац.\n";
    let n = create_note(&db, note(space, "descr", src)).await.unwrap();
    assert_eq!(n.l0.as_deref(), Some("Описание"));
    assert_eq!(n.l0_source.as_deref(), Some("manual"));

    let long = format!("# Заголовок\n\n{}\n", "слово ".repeat(60));
    let n = create_note(&db, note(space, "auto", &long)).await.unwrap();
    let l0 = n.l0.unwrap();
    assert_eq!(n.l0_source.as_deref(), Some("auto"));
    assert!(l0.chars().count() <= 200, "{}", l0.chars().count());
    assert!(l0.starts_with("слово слово"));
    assert!(
        l0.split(' ').all(|w| w == "слово"),
        "обрезка по границе слова: {l0}"
    );

    let n = create_note(&db, note(space, "short", "Коротко.\n"))
        .await
        .unwrap();
    assert_eq!(n.l0.as_deref(), Some("Коротко."));
    assert_eq!(n.l0_source.as_deref(), Some("auto"));

    let n = create_note(&db, note(space, "empty", "# Только заголовок\n"))
        .await
        .unwrap();
    assert!(n.l0.is_none() && n.l0_source.is_none());
}

// ---------- миграция ----------

#[tokio::test]
async fn migrate_from_v1() {
    let dd = datadir("migv1");
    let user = Id::new();
    let path = dd.user_db(&user);
    let sid = Id::new();
    {
        let db = Db::open(&path, &USER_MIGRATIONS[..1]).unwrap();
        db.write(move |tx| {
            tx.execute(
                "INSERT INTO spaces (id, kind, solution_guid, name, created_at)
                 VALUES (?1, 'solution', 'g-1', 'Проект', '2026-10-18T00:00:00.000000Z')",
                [sid],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    }
    let db = UserDbs::new(dd.users_dir()).get(user).await.unwrap();
    assert!(path.with_file_name(format!("{user}.db.bak-v1")).exists());
    let ver: u32 = db
        .read(|c| Ok(c.query_row("PRAGMA user_version", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(ver, 2);
    let spaces = list_spaces(&db).await.unwrap();
    assert!(spaces.iter().any(|s| s.id == sid && s.name == "Проект"));
    let n = create_note(&db, note(sid, "после/миграции", "# H\n\nтекст\n"))
        .await
        .unwrap();
    assert_eq!(n.current_rev, 1);
}
