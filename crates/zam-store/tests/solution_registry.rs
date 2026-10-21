//! Регистрация решений M1–M2 в реестре `system.db`, атрибуция `user_id` ревизий.

use std::path::PathBuf;

use zam_core::{Id, SolutionGuid};
use zam_store::{
    Attribution, AuthorKind, DataDir, Db, NewNote, NodePath, SolutionRole, StoreError, SystemStore,
    USER_MIGRATIONS, UserDbs, create_note, create_solution, create_solution_with_id,
    get_node_by_path, get_solution, list_revisions, register_user_solutions, rename_solution_guid,
};

fn datadir(name: &str) -> DataDir {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("registry-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    DataDir::open(&dir).unwrap()
}

fn p(s: &str) -> NodePath {
    NodePath::parse(s).unwrap()
}

fn note(space: Id, path: &str, content: &str, a: Attribution) -> NewNote {
    NewNote {
        space_id: space,
        path: p(path),
        content: content.into(),
        l0: None,
        attrib: a,
    }
}

#[tokio::test]
async fn existing_solutions_registered_once() {
    let dd = datadir("once");
    let sys = SystemStore::open(&dd).unwrap();
    let user = sys.create_user("ivan", false).await.unwrap().id;
    let db = UserDbs::new(dd.users_dir()).get(user).await.unwrap();
    let (g1, g2) = (SolutionGuid::new_v7(), SolutionGuid::new_v7());
    let (s1, _) = create_solution(&db, g1.as_str(), "первый").await.unwrap();
    let (s2, _) = create_solution(&db, g2.as_str(), "второй").await.unwrap();
    let r = register_user_solutions(&sys, &db, user).await.unwrap();
    assert_eq!((r.registered, r.already), (2, 0));
    assert!(r.renamed.is_empty());
    let i1 = sys.get_solution(&g1).await.unwrap().unwrap();
    assert_eq!(
        (i1.owner_id, i1.space_id, i1.name.as_str()),
        (user, s1.id, "первый")
    );
    assert_eq!(
        sys.solution_by_space(s2.id).await.unwrap().unwrap().guid,
        g2
    );
    let r = register_user_solutions(&sys, &db, user).await.unwrap();
    assert_eq!((r.registered, r.already), (0, 2));
    assert!(r.renamed.is_empty());
    assert_eq!(sys.list_solutions().await.unwrap().len(), 2);
}

#[tokio::test]
async fn guid_conflict_second_user_renamed_and_pending() {
    let dd = datadir("conflict");
    let sys = SystemStore::open(&dd).unwrap();
    let dbs = UserDbs::new(dd.users_dir());
    let a = sys.create_user("a", false).await.unwrap().id;
    let b = sys.create_user("b", false).await.unwrap().id;
    let (da, db_b) = (dbs.get(a).await.unwrap(), dbs.get(b).await.unwrap());
    let g = SolutionGuid::new_v7();
    create_solution(&da, g.as_str(), "общий").await.unwrap();
    let (sb, _) = create_solution(&db_b, g.as_str(), "мой").await.unwrap();
    let n = create_note(
        &db_b,
        note(
            sb.id,
            "план",
            "# План\n\nтекст b",
            Attribution::new(AuthorKind::Agent),
        ),
    )
    .await
    .unwrap();
    // порядок — по созданию пользователей: первый зарегистрировавший — владелец
    register_user_solutions(&sys, &da, a).await.unwrap();
    let r = register_user_solutions(&sys, &db_b, b).await.unwrap();
    assert_eq!((r.registered, r.already), (1, 0));
    assert_eq!(r.renamed.len(), 1);
    let (old, new) = r.renamed[0].clone();
    assert_eq!(old, g);
    assert_ne!(new, g);
    // у второго пространство то же, GUID новый, заметки на месте
    assert!(get_solution(&db_b, g.as_str()).await.unwrap().is_none());
    let renamed = get_solution(&db_b, new.as_str()).await.unwrap().unwrap();
    assert_eq!(renamed.id, sb.id);
    assert_eq!(renamed.solution_guid.as_deref(), Some(new.as_str()));
    assert_eq!(
        get_node_by_path(&db_b, sb.id, &p("план")).await.unwrap().id,
        n.id
    );
    // реестр: исходный GUID за первым, новый — за вторым, запрос второго к исходному
    assert_eq!(sys.get_solution(&g).await.unwrap().unwrap().owner_id, a);
    let ni = sys.get_solution(&new).await.unwrap().unwrap();
    assert_eq!((ni.owner_id, ni.space_id), (b, sb.id));
    assert_eq!(
        sys.solution_role(&g, b).await.unwrap(),
        Some(SolutionRole::Pending)
    );
    assert_eq!(sys.pending_requests_for_owner(a).await.unwrap(), 1);
    let audit = sys.list_audit(100, None).await.unwrap();
    let conflict = audit
        .iter()
        .find(|r| r.action == "solution.guid_conflict")
        .expect("аудит конфликта");
    assert_eq!(conflict.user_id, Some(b));
    assert_eq!(conflict.target.as_deref(), Some(g.as_str()));
    assert_eq!(conflict.details["new_guid"], new.as_str());
    // заметка переиндексируется с новым GUID в payload
    let jobs: i64 = db_b
        .read(move |c| {
            Ok(c.query_row(
                "SELECT count(*) FROM index_jobs WHERE target_id = ?1 AND state = 'queued'",
                [n.id],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(jobs, 1);
    // идемпотентно
    let r = register_user_solutions(&sys, &db_b, b).await.unwrap();
    assert_eq!((r.registered, r.already), (0, 1));
    assert!(r.renamed.is_empty());
    let r = register_user_solutions(&sys, &da, a).await.unwrap();
    assert_eq!((r.registered, r.already), (0, 1));
}

#[tokio::test]
async fn guid_conflict_resumes_after_interrupted_rename() {
    // реестр уже записан, а переименование в базе пользователя не состоялось (авария)
    let dd = datadir("resume");
    let sys = SystemStore::open(&dd).unwrap();
    let dbs = UserDbs::new(dd.users_dir());
    let a = sys.create_user("a", false).await.unwrap().id;
    let b = sys.create_user("b", false).await.unwrap().id;
    let db_b = dbs.get(b).await.unwrap();
    let g = SolutionGuid::new_v7();
    sys.register_solution(&g, a, Id::new(), "общий")
        .await
        .unwrap();
    let (sb, _) = create_solution(&db_b, g.as_str(), "мой").await.unwrap();
    let new = SolutionGuid::new_v7();
    sys.register_solution(&new, b, sb.id, "мой").await.unwrap();
    let r = register_user_solutions(&sys, &db_b, b).await.unwrap();
    assert_eq!(r.renamed, vec![(g.clone(), new.clone())]);
    assert_eq!(
        get_solution(&db_b, new.as_str()).await.unwrap().unwrap().id,
        sb.id
    );
    assert_eq!(sys.list_solutions().await.unwrap().len(), 2);
}

#[tokio::test]
async fn create_with_id_and_rename() {
    let dd = datadir("withid");
    let db = UserDbs::new(dd.users_dir()).get(Id::new()).await.unwrap();
    let (space, g) = (Id::new(), SolutionGuid::new_v7());
    let (s, created) = create_solution_with_id(&db, space, &g, "проект")
        .await
        .unwrap();
    assert!(created);
    assert_eq!(
        (s.id, s.solution_guid.as_deref()),
        (space, Some(g.as_str()))
    );
    let (s2, created) = create_solution_with_id(&db, space, &g, "другое")
        .await
        .unwrap();
    assert!(!created);
    assert_eq!(s2, s);
    // id занят другим GUID
    let e = create_solution_with_id(&db, space, &SolutionGuid::new_v7(), "x")
        .await
        .unwrap_err();
    assert!(matches!(e, StoreError::Conflict(_)), "{e:?}");
    // GUID занят другим id
    let e = create_solution_with_id(&db, Id::new(), &g, "x")
        .await
        .unwrap_err();
    assert!(matches!(e, StoreError::Conflict(_)), "{e:?}");
    let g2 = SolutionGuid::new_v7();
    rename_solution_guid(&db, space, &g2).await.unwrap();
    assert_eq!(
        get_solution(&db, g2.as_str()).await.unwrap().unwrap().id,
        space
    );
    let e = rename_solution_guid(&db, Id::new(), &g2).await.unwrap_err();
    assert!(matches!(e, StoreError::NotFound), "{e:?}");
    let (other, _) = create_solution_with_id(&db, Id::new(), &g, "x")
        .await
        .unwrap();
    let e = rename_solution_guid(&db, other.id, &g2).await.unwrap_err();
    assert!(matches!(e, StoreError::Conflict(_)), "{e:?}");
}

#[tokio::test]
async fn revision_records_user_id() {
    let dd = datadir("userid");
    let db = UserDbs::new(dd.users_dir()).get(Id::new()).await.unwrap();
    let (space, _) = create_solution(&db, SolutionGuid::new_v7().as_str(), "p")
        .await
        .unwrap();
    let member = Id::new();
    let mut a = Attribution::new(AuthorKind::Agent);
    assert_eq!(a.user_id, None);
    a.user_id = Some(member);
    let n = create_note(&db, note(space.id, "x", "текст участника", a))
        .await
        .unwrap();
    let own = create_note(
        &db,
        note(
            space.id,
            "y",
            "текст владельца",
            Attribution::new(AuthorKind::Human),
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        list_revisions(&db, n.id).await.unwrap()[0].user_id,
        Some(member)
    );
    assert_eq!(list_revisions(&db, own.id).await.unwrap()[0].user_id, None);
    let raw: Option<String> = db
        .read(move |c| {
            Ok(c.query_row(
                "SELECT user_id FROM revisions WHERE node_id = ?1",
                [n.id],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(raw, Some(member.to_string()));
}

#[tokio::test]
async fn migration_v6_to_v7_keeps_revisions() {
    let dd = datadir("migv6");
    let user = Id::new();
    let path = dd.user_db(&user);
    let (space, node) = (Id::new(), Id::new());
    {
        let db = Db::open(&path, &USER_MIGRATIONS[..6]).unwrap();
        db.write(move |tx| {
            let now = "2026-10-18T00:00:00.000000Z";
            tx.execute(
                "INSERT INTO spaces (id, kind, name, created_at)
                 VALUES (?1, 'knowledge', 'Knowledge', ?2)",
                rusqlite::params![space, now],
            )?;
            tx.execute(
                "INSERT INTO nodes (id, space_id, kind, name, path, current_rev,
                                    index_state, created_at, updated_at)
                 VALUES (?1, ?2, 'note', 'n', 'n', 1, 'indexed', ?3, ?3)",
                rusqlite::params![node, space, now],
            )?;
            tx.execute(
                "INSERT INTO revisions (node_id, rev, content, content_hash, size,
                                        author_kind, message, ts)
                 VALUES (?1, 1, 'старый текст', 'h', 1, 'agent', 'm2', ?2)",
                rusqlite::params![node, now],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    }
    let db = UserDbs::new(dd.users_dir()).get(user).await.unwrap();
    assert!(path.with_file_name(format!("{user}.db.bak-v6")).exists());
    let ver: u32 = db
        .read(|c| Ok(c.query_row("PRAGMA user_version", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(ver, USER_MIGRATIONS.len() as u32);
    let revs = list_revisions(&db, node).await.unwrap();
    assert_eq!(revs.len(), 1);
    assert_eq!(
        (revs[0].message.as_deref(), revs[0].user_id),
        (Some("m2"), None)
    );
    // ревизии по-прежнему только дописываются
    let upd = db
        .write(move |tx| {
            tx.execute(
                "UPDATE revisions SET user_id = 'x' WHERE node_id = ?1",
                [node],
            )?;
            Ok(())
        })
        .await;
    assert!(upd.is_err());
}
