use std::path::PathBuf;

use zam_core::Id;
use zam_markdown::{PatchError, PatchOp};
use zam_store::{
    Attribution, AuthorKind, DataDir, Db, NewNote, NodeKind, NodePath, SpaceKind, StoreError,
    USER_MIGRATIONS, UserDbs, WriteError, create_folder, create_note, delete_node, find_by_name,
    get_node, get_node_by_path, list_children, list_revisions, list_spaces, list_trash, move_node,
    node_content, patch_note, restore_node, revert_note, unified_diff, update_note,
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
        user_id: None,
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

/// Число листовых чанков (`level = 'chunk'`) заметки.
async fn chunk_count(db: &Db, node: Id) -> i64 {
    count(
        db,
        "SELECT count(*) FROM chunks WHERE node_id = ?1 AND level = 'chunk'",
        node.to_string(),
    )
    .await
}

/// `(heading_path, text)` листовых чанков заметки по порядку.
async fn chunk_rows(db: &Db, node: Id) -> Vec<(String, String)> {
    db.read(move |c| {
        let mut st = c.prepare(
            "SELECT heading_path, text FROM chunks
             WHERE node_id = ?1 AND level = 'chunk' ORDER BY ordinal",
        )?;
        let rows = st.query_map([node], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    })
    .await
    .unwrap()
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

/// Чанкер §8.2: короткие секции-соседи склеиваются, за листами (`chunk`) идёт L1
/// заметки (`note`); в FTS — оба уровня.
#[tokio::test]
async fn chunks_by_section_rules_and_fts() {
    let (db, space) = setup("chunks").await;
    let n = create_note(&db, note(space, "Инструменты/Bash скрипты", THREE))
        .await
        .unwrap();
    assert_eq!(chunk_count(&db, n.id).await, 2);

    let rows: Vec<(String, i64, String, String, String, String, i64)> = db
        .read(move |c| {
            let mut st = c.prepare(
                "SELECT heading_path, ordinal, text, context_prefix, level, text_hash, token_count
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
                    r.get(6)?,
                ))
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
        .unwrap();
    let heads: Vec<_> = rows.iter().map(|r| (r.0.as_str(), r.4.as_str())).collect();
    assert_eq!(
        heads,
        vec![
            ("Обзор", "chunk"),
            ("Обзор > Установка", "chunk"),
            ("", "note")
        ]
    );
    assert_eq!(rows.iter().map(|r| r.1).collect::<Vec<_>>(), vec![0, 1, 2]);
    // «Установка» и «Использование» — короткие соседи: один лист, путь первой секции,
    // строка заголовка присоединённой секции остаётся в тексте
    assert!(!rows[1].2.contains("## Установка"));
    assert!(rows[1].2.contains("Ставим скрипты") && rows[1].2.contains("#### Детали"));
    assert!(rows[1].2.contains("## Использование"));
    assert_eq!(
        rows[1].3,
        "[Knowledge/Инструменты/Bash скрипты] Bash скрипты > Обзор > Установка"
    );
    assert!(rows.iter().all(|r| r.5.len() == 64 && r.6 > 0));

    let hits = count(
        &db,
        "SELECT count(*) FROM chunks_fts WHERE chunks_fts MATCH ?1",
        "скрипты".into(),
    )
    .await;
    // имя заметки «Bash скрипты» входит в context_prefix каждой строки FTS
    assert_eq!(hits, 3);
    assert_eq!(fts_hits(&db, n.id, "скрипты").await, 1);
    let tri = count(
        &db,
        "SELECT count(*) FROM chunks_fts_tri f JOIN chunks c ON c.seq = f.rowid
         WHERE chunks_fts_tri MATCH ?1 AND c.level = 'chunk'",
        "\"core.autocrlf\"".into(),
    )
    .await;
    assert_eq!(tri, 1);
    let by_heading = count(
        &db,
        "SELECT count(*) FROM chunks_fts WHERE chunks_fts MATCH ?1",
        "heading_path : установка".into(),
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
                 WHERE node_id = ?1 AND level = 'chunk' ORDER BY ordinal",
            )?;
            let rows = st.query_map([n.id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
        .unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0].0, "");
    assert!(rows[0].1.starts_with("Вступление") && !rows[0].1.contains("tags"));
    assert_eq!(rows[0].2, "[Knowledge/Пре] Пре | a");
    assert_eq!(rows[1].2, "[Knowledge/Пре] Пре > Полная | a");
    assert_eq!(rows[1].0, "Полная");
    assert_eq!(
        n.frontmatter,
        Some(serde_json::json!({"tags": ["a"]})),
        "null-поля опускаются"
    );
}

#[tokio::test]
async fn fts_finds_by_name_path_and_tag() {
    let (db, space) = setup("prefix").await;
    let src = "---
tags: [девопс, linux]
---
# Раздел

просто текст
";
    let n = create_note(&db, note(space, "Инструменты/Шпаргалка", src))
        .await
        .unwrap();
    let rows: i64 = chunk_count(&db, n.id).await;
    assert_eq!(rows, 1);
    let prefix: String = db
        .read(move |c| {
            Ok(c.query_row(
                "SELECT context_prefix FROM chunks WHERE node_id = ?1 AND level = 'chunk'",
                [n.id],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(
        prefix,
        "[Knowledge/Инструменты/Шпаргалка] Шпаргалка > Раздел | девопс, linux"
    );
    for q in [
        "шпаргалка",
        "инструменты",
        "девопс",
        "LINUX",
        "context_prefix : knowledge",
    ] {
        let hits = count(
            &db,
            "SELECT count(*) FROM chunks_fts f JOIN chunks c ON c.seq = f.rowid
             WHERE chunks_fts MATCH ?1 AND c.level = 'chunk'",
            q.into(),
        )
        .await;
        assert_eq!(hits, 1, "запрос {q:?}");
    }
}

#[tokio::test]
async fn index_false_keeps_fts_and_marks_excluded() {
    // `index: false` исключает только из векторного индекса (§6.4): FTS остаётся
    let (db, space) = setup("noindex").await;
    let src = "---\nindex: false\n---\n# Секрет\n\nскрипты\n";
    let n = create_note(&db, note(space, "x", src)).await.unwrap();
    assert_eq!(chunk_count(&db, n.id).await, 1);
    assert_eq!(n.frontmatter.unwrap()["index"], serde_json::json!(false));
    assert_eq!(n.index_state, "excluded", "M2 пропустит векторизацию");
    assert_eq!(fts_hits(&db, n.id, "скрипты").await, 1);
}

#[tokio::test]
async fn note_without_own_text_still_has_chunk() {
    let (db, space) = setup("emptynote").await;
    // заметка из WebUI: только заголовок
    let n = create_note(&db, note(space, "Новая", "# Новая заметка\n"))
        .await
        .unwrap();
    let rows = chunk_rows(&db, n.id).await;
    assert_eq!(
        rows,
        vec![("Новая заметка".to_string(), "Новая заметка".to_string())]
    );
    // совсем пустая — чанк с именем узла
    let n = create_note(&db, note(space, "Пустышка", "")).await.unwrap();
    assert_eq!(
        chunk_rows(&db, n.id).await,
        vec![(String::new(), "Пустышка".to_string())]
    );
    // только пустые секции — первый заголовок
    let n = create_note(&db, note(space, "Пустые", "# Раз\n\n## Два\n   \n"))
        .await
        .unwrap();
    assert_eq!(
        chunk_rows(&db, n.id).await,
        vec![("Раз".to_string(), "Раз".to_string())]
    );
    for q in ["заметка", "пустышка", "раз"] {
        let hits = count(
            &db,
            "SELECT count(*) FROM chunks_fts WHERE chunks_fts MATCH ?1",
            q.into(),
        )
        .await;
        assert!(hits >= 1, "запрос {q:?}");
    }
}

#[tokio::test]
async fn revisions_are_append_only_in_schema() {
    let (db, space) = setup("appendonly").await;
    let n = create_note(&db, note(space, "a", "# A\n\nтекст\n"))
        .await
        .unwrap();
    for sql in [
        "UPDATE revisions SET content = 'подмена' WHERE node_id = ?1",
        "DELETE FROM revisions WHERE node_id = ?1",
    ] {
        let err = db
            .write(move |t| Ok(t.execute(sql, [n.id])?))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("revisions are append-only"),
            "{sql}: {err}"
        );
    }
    assert_eq!(
        node_content(&db, n.id, None).await.unwrap(),
        (1, "# A\n\nтекст\n".to_string())
    );
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
    assert_eq!(ver, zam_store::USER_MIGRATIONS.len() as u32);
    let spaces = list_spaces(&db).await.unwrap();
    assert!(spaces.iter().any(|s| s.id == sid && s.name == "Проект"));
    let n = create_note(&db, note(sid, "после/миграции", "# H\n\nтекст\n"))
        .await
        .unwrap();
    assert_eq!(n.current_rev, 1);
}

// ---------- правка, патч, история, откат ----------

const TWO: &str = "# Альфа\n\nстарое слово\n\n# Бета\n\nвторая секция\n";

fn human(msg: &str) -> Attribution {
    Attribution {
        message: Some(msg.into()),
        user_id: None,
        ..Attribution::new(AuthorKind::Human)
    }
}

fn replace(section: &str, content: &str) -> PatchOp {
    PatchOp::ReplaceSection {
        section: section.into(),
        content: content.into(),
    }
}

/// Число листовых чанков заметки, в тексте которых FTS находит слово.
async fn fts_hits(db: &Db, node: Id, word: &str) -> i64 {
    let q = format!("text : {word}");
    db.read(move |c| {
        Ok(c.query_row(
            "SELECT count(*) FROM chunks_fts f JOIN chunks c ON c.seq = f.rowid
             WHERE chunks_fts MATCH ?1 AND c.node_id = ?2 AND c.level = 'chunk'",
            rusqlite::params![q, node],
            |r| r.get(0),
        )?)
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn update_with_right_rev_rebuilds_chunks() {
    let (db, space) = setup("update").await;
    let n = create_note(&db, note(space, "Заметка", TWO)).await.unwrap();
    assert_eq!(fts_hits(&db, n.id, "старое").await, 1);

    let new = TWO.replace("старое", "новое");
    let u = update_note(&db, n.id, 1, new.clone(), None, human("правка"))
        .await
        .unwrap();
    assert_eq!(u.current_rev, 2);
    assert_ne!(u.content_hash, n.content_hash);
    assert!(u.updated_at >= n.updated_at);
    assert_eq!(
        node_content(&db, n.id, None).await.unwrap(),
        (2, new.clone())
    );
    assert_eq!(fts_hits(&db, n.id, "старое").await, 0);
    assert_eq!(fts_hits(&db, n.id, "новое").await, 1);

    let revs = list_revisions(&db, n.id).await.unwrap();
    assert_eq!(revs.iter().map(|r| r.rev).collect::<Vec<_>>(), vec![2, 1]);
    assert_eq!(revs[0].author_kind, "human");
    assert_eq!(revs[0].message.as_deref(), Some("правка"));
    assert_eq!(revs[0].content_hash, u.content_hash.unwrap());
    assert_eq!(revs[0].size, new.len() as i64);
    assert_eq!(revs[1].author_kind, "agent");
    assert_eq!(revs[1].machine.as_deref(), Some("box"));
    assert_eq!(revs[1].session_id.as_deref(), Some("s1"));
    assert!(revs[1].token_id.is_some());
    assert!(revs[0].ts >= revs[1].ts);
    assert!(matches!(
        list_revisions(&db, Id::new()).await,
        Err(StoreError::NotFound)
    ));
}

#[tokio::test]
async fn update_l0_rules() {
    let (db, space) = setup("update-l0").await;
    let mut nn = note(space, "Л", "первый абзац\n");
    nn.l0 = Some("от агента".into());
    let n = create_note(&db, nn).await.unwrap();
    assert_eq!(n.l0_source.as_deref(), Some("agent"));
    // смена текста без нового l0 — сводка агента не сохраняется (§8.3)
    let u = update_note(&db, n.id, 1, "другой абзац\n".into(), None, human("x"))
        .await
        .unwrap();
    assert_eq!(
        (u.l0.as_deref(), u.l0_source.as_deref()),
        (Some("другой абзац"), Some("auto"))
    );
    let u = update_note(
        &db,
        n.id,
        2,
        "третий\n".into(),
        Some("снова агент".into()),
        human("x"),
    )
    .await
    .unwrap();
    assert_eq!(
        (u.l0.as_deref(), u.l0_source.as_deref()),
        (Some("снова агент"), Some("agent"))
    );
}

#[tokio::test]
async fn update_with_stale_rev_is_mismatch() {
    let (db, space) = setup("update-stale").await;
    let n = create_note(&db, note(space, "Заметка", TWO)).await.unwrap();
    update_note(&db, n.id, 1, "v2".into(), None, human("a"))
        .await
        .unwrap();
    match update_note(&db, n.id, 1, "v3".into(), None, human("b")).await {
        Err(WriteError::RevMismatch {
            current_rev,
            current,
        }) => assert_eq!((current_rev, current.as_str()), (2, "v2")),
        other => panic!("ожидался RevMismatch: {other:?}"),
    }
    assert!(matches!(
        update_note(&db, n.id, 3, "v3".into(), None, human("b")).await,
        Err(WriteError::RevMismatch { current_rev: 2, .. })
    ));
    assert_eq!(list_revisions(&db, n.id).await.unwrap().len(), 2);
}

#[tokio::test]
async fn same_content_creates_no_revision() {
    let (db, space) = setup("same").await;
    let n = create_note(&db, note(space, "Заметка", TWO)).await.unwrap();
    let u = update_note(&db, n.id, 1, TWO.into(), None, human("тот же"))
        .await
        .unwrap();
    assert_eq!(u, n);
    let u = patch_note(
        &db,
        n.id,
        1,
        // тело секции начинается сразу после строки заголовка
        vec![replace("Бета", "\nвторая секция\n")],
        human("p"),
    )
    .await
    .unwrap();
    assert_eq!(u, n);
    assert_eq!(list_revisions(&db, n.id).await.unwrap().len(), 1);
}

#[tokio::test]
async fn patch_other_section_with_old_base_rev() {
    let (db, space) = setup("patch-other").await;
    let n = create_note(&db, note(space, "Заметка", TWO)).await.unwrap();
    // другая сторона меняет «Бету» (rev 2)
    patch_note(
        &db,
        n.id,
        1,
        vec![replace("Бета", "бета от машины Б\n")],
        human("б"),
    )
    .await
    .unwrap();
    // эта сторона правит «Альфу» на base_rev = 1
    let u = patch_note(
        &db,
        n.id,
        1,
        vec![
            replace("Альфа", "альфа от машины А\n"),
            PatchOp::Append {
                content: "хвост\n".into(),
            },
        ],
        human("а"),
    )
    .await
    .unwrap();
    assert_eq!(u.current_rev, 3);
    let (_, text) = node_content(&db, n.id, None).await.unwrap();
    assert!(text.contains("альфа от машины А"), "{text}");
    assert!(text.contains("бета от машины Б"), "{text}");
    assert!(text.ends_with("хвост\n"), "{text}");
    assert!(!text.contains("старое слово"));
    // обе короткие секции — один лист (§8.2)
    assert_eq!(fts_hits(&db, n.id, "машины").await, 1);
}

#[tokio::test]
async fn patch_changed_section_is_conflict() {
    let (db, space) = setup("patch-same").await;
    let n = create_note(&db, note(space, "Заметка", TWO)).await.unwrap();
    patch_note(&db, n.id, 1, vec![replace("Бета", "Б\n")], human("б"))
        .await
        .unwrap();
    match patch_note(&db, n.id, 1, vec![replace("Бета", "А\n")], human("а")).await {
        Err(WriteError::SectionChanged {
            section,
            current_rev,
        }) => assert_eq!((section.as_str(), current_rev), ("Бета", 2)),
        other => panic!("ожидался SectionChanged: {other:?}"),
    }
    // неудачная операция в цепочке — ничего не записано
    let r = patch_note(
        &db,
        n.id,
        2,
        vec![replace("Альфа", "x\n"), replace("Нет такой", "y\n")],
        human("а"),
    )
    .await;
    assert!(
        matches!(r, Err(WriteError::Patch(PatchError::SectionNotFound(_)))),
        "{r:?}"
    );
    // base_rev из будущего и несуществующий base_rev
    assert!(matches!(
        patch_note(&db, n.id, 5, vec![replace("Альфа", "x\n")], human("а")).await,
        Err(WriteError::RevMismatch { current_rev: 2, .. })
    ));
    assert!(matches!(
        patch_note(&db, n.id, 0, vec![replace("Альфа", "x\n")], human("а")).await,
        Err(WriteError::Store(StoreError::NotFound))
    ));
    assert_eq!(get_node(&db, n.id).await.unwrap().current_rev, 2);
    // папка — BadInput, нет узла — NotFound
    let f = create_folder(&db, space, p("Папка")).await.unwrap();
    assert!(matches!(
        update_note(&db, f.id, 0, "x".into(), None, human("а")).await,
        Err(WriteError::Store(StoreError::BadInput(_)))
    ));
    assert!(matches!(
        update_note(&db, Id::new(), 1, "x".into(), None, human("а")).await,
        Err(WriteError::Store(StoreError::NotFound))
    ));
}

#[tokio::test]
async fn deleted_note_is_not_writable() {
    let (db, space) = setup("deleted").await;
    let n = create_note(&db, note(space, "Заметка", TWO)).await.unwrap();
    db.write(move |tx| {
        tx.execute(
            "UPDATE nodes SET deleted_at = '2026-10-18T00:00:00.000000Z' WHERE id = ?1",
            [n.id],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    assert!(matches!(
        update_note(&db, n.id, 1, "x".into(), None, human("а")).await,
        Err(WriteError::Store(StoreError::NotFound))
    ));
    assert!(matches!(
        patch_note(&db, n.id, 1, vec![replace("Альфа", "x\n")], human("а")).await,
        Err(WriteError::Store(StoreError::NotFound))
    ));
    assert!(matches!(
        revert_note(&db, n.id, 1, human("а")).await,
        Err(StoreError::NotFound)
    ));
}

#[tokio::test]
async fn revert_adds_revision_with_old_text() {
    let (db, space) = setup("revert").await;
    let n = create_note(&db, note(space, "Заметка", TWO)).await.unwrap();
    update_note(&db, n.id, 1, "v2 новое\n".into(), None, human("2"))
        .await
        .unwrap();
    update_note(&db, n.id, 2, "v3\n".into(), None, human("3"))
        .await
        .unwrap();
    let r = revert_note(&db, n.id, 1, human("откат к 1")).await.unwrap();
    assert_eq!(r.current_rev, 4);
    assert_eq!(r.content_hash, n.content_hash);
    assert_eq!(r.l0_source.as_deref(), Some("auto"));
    assert_eq!(
        node_content(&db, n.id, None).await.unwrap(),
        (4, TWO.to_string())
    );
    let revs = list_revisions(&db, n.id).await.unwrap();
    assert_eq!(revs.len(), 4);
    assert_eq!(revs[0].message.as_deref(), Some("откат к 1"));
    assert_eq!(fts_hits(&db, n.id, "старое").await, 1);
    // откат к содержимому текущей ревизии — без новой ревизии
    let same = revert_note(&db, n.id, 1, human("ещё раз")).await.unwrap();
    assert_eq!(same.current_rev, 4);
    assert_eq!(list_revisions(&db, n.id).await.unwrap().len(), 4);
    assert!(matches!(
        revert_note(&db, n.id, 9, human("x")).await,
        Err(StoreError::NotFound)
    ));
}

#[test]
fn unified_diff_marks_lines() {
    let d = unified_diff("a\nстарое\nb\n", "a\nновое\nb\n", "rev 1", "rev 2");
    assert!(d.starts_with("--- rev 1\n+++ rev 2\n"), "{d}");
    assert!(d.contains("@@ -1,3 +1,3 @@"), "{d}");
    assert!(d.contains("\n-старое\n"), "{d}");
    assert!(d.contains("\n+новое\n"), "{d}");
    // контекст 3 строки: дальние строки в ханк не попадают
    let old = "1\n2\n3\n4\n5\n6\n7\n8\n9\n";
    let d = unified_diff(old, &old.replace("9\n", "девять\n"), "a", "b");
    assert!(d.contains("@@ -6,4 +6,4 @@") && !d.contains(" 5\n"), "{d}");
    assert_eq!(unified_diff("x\n", "x\n", "a", "b"), "");
}

// ---------- корзина и перемещение ----------

const WORDY: &str = "# Раздел\n\nуникальнослово здесь\n";

/// Число листовых чанков заметки, у которых FTS находит слово в `context_prefix`.
async fn prefix_hits(db: &Db, node: Id, word: &str) -> i64 {
    let q = format!("context_prefix : {word}");
    db.read(move |c| {
        Ok(c.query_row(
            "SELECT count(*) FROM chunks_fts f JOIN chunks c ON c.seq = f.rowid
             WHERE chunks_fts MATCH ?1 AND c.node_id = ?2 AND c.level = 'chunk'",
            rusqlite::params![q, node],
            |r| r.get(0),
        )?)
    })
    .await
    .unwrap()
}

fn ids(nodes: &[zam_store::Node]) -> Vec<Id> {
    nodes.iter().map(|n| n.id).collect()
}

#[tokio::test]
async fn delete_folder_and_restore_cycle() {
    let (db, space) = setup("trash").await;
    let x = create_note(&db, note(space, "a/x", WORDY)).await.unwrap();
    let y = create_note(&db, note(space, "a/y", WORDY)).await.unwrap();
    let a = get_node_by_path(&db, space, &p("a")).await.unwrap();

    assert_eq!(delete_node(&db, a.id).await.unwrap(), 3);
    let mut stamps = Vec::new();
    for id in [a.id, x.id, y.id] {
        stamps.push(get_node(&db, id).await.unwrap().deleted_at.expect("удалён"));
    }
    assert!(stamps.iter().all(|t| *t == stamps[0]), "одна метка времени");
    assert_eq!(fts_hits(&db, x.id, "уникальнослово").await, 0);
    assert_eq!(chunk_count(&db, y.id).await, 0);
    assert!(matches!(
        get_node_by_path(&db, space, &p("a")).await,
        Err(StoreError::NotFound)
    ));
    // повторное удаление — NotFound
    assert!(matches!(
        delete_node(&db, a.id).await,
        Err(StoreError::NotFound)
    ));

    // путь свободен: новая папка с тем же путём
    let a2 = create_folder(&db, space, p("a")).await.unwrap();
    assert!(matches!(
        restore_node(&db, a.id).await,
        Err(StoreError::Conflict(_))
    ));
    // ребёнок тянет за собой предка, путь которого занят
    assert!(matches!(
        restore_node(&db, x.id).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(get_node(&db, x.id).await.unwrap().deleted_at.is_some());

    assert_eq!(
        ids(&list_trash(&db, Some(space)).await.unwrap()),
        vec![a.id]
    );

    assert_eq!(delete_node(&db, a2.id).await.unwrap(), 1);
    assert_eq!(
        ids(&list_trash(&db, None).await.unwrap()),
        vec![a2.id, a.id],
        "новые удаления первыми"
    );

    let r = restore_node(&db, a.id).await.unwrap();
    assert_eq!(r.id, a.id);
    assert!(r.deleted_at.is_none());
    assert!(r.has_children);
    for id in [x.id, y.id] {
        assert!(get_node(&db, id).await.unwrap().deleted_at.is_none());
        assert_eq!(fts_hits(&db, id, "уникальнослово").await, 1);
    }
    assert_eq!(
        get_node_by_path(&db, space, &p("a/x")).await.unwrap().id,
        x.id
    );
    // живой узел восстанавливать нечего
    assert!(matches!(
        restore_node(&db, a.id).await,
        Err(StoreError::BadInput(_))
    ));
    assert!(matches!(
        restore_node(&db, Id::new()).await,
        Err(StoreError::NotFound)
    ));
    assert_eq!(
        ids(&list_trash(&db, Some(space)).await.unwrap()),
        vec![a2.id]
    );
    assert!(list_trash(&db, Some(Id::new())).await.unwrap().is_empty());
}

#[tokio::test]
async fn restore_child_restores_only_ancestors() {
    let (db, space) = setup("trash-child").await;
    let x = create_note(&db, note(space, "a/b/x", WORDY)).await.unwrap();
    let y = create_note(&db, note(space, "a/y", WORDY)).await.unwrap();
    let a = get_node_by_path(&db, space, &p("a")).await.unwrap();
    let b = get_node_by_path(&db, space, &p("a/b")).await.unwrap();
    assert_eq!(delete_node(&db, a.id).await.unwrap(), 4);

    restore_node(&db, x.id).await.unwrap();
    for id in [a.id, b.id, x.id] {
        assert!(get_node(&db, id).await.unwrap().deleted_at.is_none());
    }
    assert!(get_node(&db, y.id).await.unwrap().deleted_at.is_some());
    assert_eq!(fts_hits(&db, x.id, "уникальнослово").await, 1);
    // y теперь корень удалённого: его родитель жив
    assert_eq!(
        ids(&list_trash(&db, Some(space)).await.unwrap()),
        vec![y.id]
    );
}

#[tokio::test]
async fn earlier_deleted_descendant_stays_deleted() {
    let (db, space) = setup("trash-earlier").await;
    let x = create_note(&db, note(space, "a/x", WORDY)).await.unwrap();
    let y = create_note(&db, note(space, "a/y", WORDY)).await.unwrap();
    let a = get_node_by_path(&db, space, &p("a")).await.unwrap();
    assert_eq!(delete_node(&db, y.id).await.unwrap(), 1);
    assert_eq!(delete_node(&db, a.id).await.unwrap(), 2);

    assert_eq!(
        ids(&list_trash(&db, Some(space)).await.unwrap()),
        vec![a.id, y.id]
    );
    restore_node(&db, a.id).await.unwrap();
    assert!(get_node(&db, x.id).await.unwrap().deleted_at.is_none());
    assert!(get_node(&db, y.id).await.unwrap().deleted_at.is_some());
    assert_eq!(chunk_count(&db, y.id).await, 0);
    assert_eq!(
        ids(&list_trash(&db, Some(space)).await.unwrap()),
        vec![y.id]
    );
}

#[tokio::test]
async fn move_folder_rewrites_descendants() {
    let (db, space) = setup("move").await;
    let x = create_note(&db, note(space, "a/x", WORDY)).await.unwrap();
    let z = create_note(&db, note(space, "a/sub/z", WORDY))
        .await
        .unwrap();
    let a = get_node_by_path(&db, space, &p("a")).await.unwrap();
    let sub = get_node_by_path(&db, space, &p("a/sub")).await.unwrap();

    let m = move_node(&db, a.id, p("b/c")).await.unwrap();
    assert_eq!((m.path.as_str(), m.name.as_str()), ("b/c", "c"));
    let b = get_node_by_path(&db, space, &p("b")).await.unwrap();
    assert_eq!(b.kind, NodeKind::Folder);
    assert_eq!(m.parent_id, Some(b.id));
    assert!(m.updated_at >= a.updated_at);

    for (id, path) in [(x.id, "b/c/x"), (sub.id, "b/c/sub"), (z.id, "b/c/sub/z")] {
        assert_eq!(get_node(&db, id).await.unwrap().path, path);
        assert_eq!(get_node_by_path(&db, space, &p(path)).await.unwrap().id, id);
    }
    assert_eq!(get_node(&db, z.id).await.unwrap().parent_id, Some(sub.id));
    assert!(matches!(
        get_node_by_path(&db, space, &p("a/x")).await,
        Err(StoreError::NotFound)
    ));

    // context_prefix пересчитан и в чанках, и в FTS
    let prefix: String = db
        .read(move |c| {
            Ok(c.query_row(
                "SELECT context_prefix FROM chunks WHERE node_id = ?1 AND level = 'chunk'",
                [z.id],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(prefix, "[Knowledge/b/c/sub/z] z > Раздел");
    assert_eq!(prefix_hits(&db, z.id, "c").await, 1);
    assert_eq!(prefix_hits(&db, x.id, "a").await, 0);
    assert_eq!(fts_hits(&db, x.id, "уникальнослово").await, 1);
}

#[tokio::test]
async fn move_rename_and_errors() {
    let (db, space) = setup("move-errors").await;
    let x = create_note(&db, note(space, "a/x", WORDY)).await.unwrap();
    let w = create_note(&db, note(space, "a/w", WORDY)).await.unwrap();
    let a = get_node_by_path(&db, space, &p("a")).await.unwrap();

    // перенос в собственное поддерево
    assert!(matches!(
        move_node(&db, a.id, p("a/z")).await,
        Err(StoreError::BadInput(_))
    ));
    assert!(matches!(
        move_node(&db, x.id, p("a/w")).await,
        Err(StoreError::Conflict(_))
    ));
    // предок назначения — заметка
    assert!(matches!(
        move_node(&db, x.id, p("a/w/q")).await,
        Err(StoreError::Conflict(_))
    ));
    assert_eq!(get_node(&db, x.id).await.unwrap().path, "a/x");
    assert!(get_node(&db, w.id).await.unwrap().deleted_at.is_none());

    let same = move_node(&db, x.id, p("a/x")).await.unwrap();
    assert_eq!(same, get_node(&db, x.id).await.unwrap());

    // переименование заметки; «a/xx» — не потомок «a/x»
    let r = move_node(&db, x.id, p("a/xx")).await.unwrap();
    assert_eq!((r.path.as_str(), r.name.as_str()), ("a/xx", "xx"));
    assert_eq!(r.parent_id, Some(a.id));
    assert_eq!(prefix_hits(&db, x.id, "xx").await, 1);

    delete_node(&db, x.id).await.unwrap();
    assert!(matches!(
        move_node(&db, x.id, p("q")).await,
        Err(StoreError::NotFound)
    ));
    assert!(matches!(
        move_node(&db, Id::new(), p("q")).await,
        Err(StoreError::NotFound)
    ));
}

#[tokio::test]
async fn index_state_in_node_and_patch_size_limit() {
    use zam_store::patch_note_within;
    let (db, space) = setup("limit").await;
    let n = create_note(&db, note(space, "lim", "# А\nтекст\n"))
        .await
        .unwrap();
    assert_eq!(n.index_state, "pending");
    let f = create_folder(&db, space, p("dir")).await.unwrap();
    assert_eq!(f.index_state, "excluded");
    let op = PatchOp::Append {
        content: "x".repeat(100),
    };
    match patch_note_within(&db, n.id, 1, vec![op.clone()], agent(), 50).await {
        Err(WriteError::TooLarge { size, max }) => assert!(size > 100 && max == 50),
        other => panic!("{:?}", other.map(|n| n.current_rev)),
    }
    assert_eq!(get_node(&db, n.id).await.unwrap().current_rev, 1);
    let u = patch_note_within(&db, n.id, 1, vec![op], agent(), 1000)
        .await
        .unwrap();
    assert_eq!(u.current_rev, 2);
}
