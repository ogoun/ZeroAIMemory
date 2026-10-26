use std::path::PathBuf;

use zam_core::Id;
use zam_store::{DataDir, SpaceKind, UserDbs, list_spaces};

fn datadir(name: &str) -> DataDir {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("userdb-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    DataDir::open(&dir).unwrap()
}

#[tokio::test]
async fn fixed_spaces_created_once() {
    let dd = datadir("fixed");
    let user = Id::new();
    for _ in 0..2 {
        let dbs = UserDbs::new(dd.users_dir());
        let db = dbs.get(user).await.unwrap();
        let sp = list_spaces(&db).await.unwrap();
        let got: Vec<_> = sp.iter().map(|s| (s.kind, s.name.as_str())).collect();
        assert_eq!(
            got,
            vec![
                (SpaceKind::Rules, "Rules"),
                (SpaceKind::Human, "Human"),
                (SpaceKind::Knowledge, "Knowledge")
            ]
        );
        assert!(sp.iter().all(|s| s.solution_guid.is_none()));
    }
}

#[tokio::test]
async fn separate_file_per_user() {
    let dd = datadir("sep");
    let dbs = UserDbs::new(dd.users_dir());
    let (a, b) = (Id::new(), Id::new());
    dbs.get(a).await.unwrap();
    dbs.get(b).await.unwrap();
    assert!(dd.user_db(&a).exists() && dd.user_db(&b).exists());
    assert_ne!(dd.user_db(&a), dd.user_db(&b));
}

#[tokio::test]
async fn concurrent_get_shares_one_db() {
    let dd = datadir("conc");
    let dbs = UserDbs::new(dd.users_dir());
    let user = Id::new();
    let hs: Vec<_> = (0..8)
        .map(|_| {
            let d = dbs.clone();
            tokio::spawn(async move { d.get(user).await.unwrap() })
        })
        .collect();
    for h in hs {
        let db = h.await.unwrap();
        assert_eq!(list_spaces(&db).await.unwrap().len(), 3);
    }
}

#[tokio::test]
async fn migrate_on_open_backup() {
    let dd = datadir("mig");
    let user = Id::new();
    let path = dd.user_db(&user);
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch("CREATE TABLE marker (x INTEGER); INSERT INTO marker VALUES (42);")
            .unwrap();
    }
    let dbs = UserDbs::new(dd.users_dir());
    let db = dbs.get(user).await.unwrap();
    let bak = path.with_file_name(format!("{user}.db.bak-v0"));
    assert!(bak.exists());
    assert_eq!(list_spaces(&db).await.unwrap().len(), 3);
    let marker: i64 = db
        .read(|c| Ok(c.query_row("SELECT x FROM marker", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(marker, 42);
}

#[tokio::test]
async fn solutions_idempotent_and_lookup() {
    use zam_store::{create_solution, get_solution, get_space, space_of_kind};
    let dd = datadir("solutions");
    let db = UserDbs::new(dd.users_dir()).get(Id::new()).await.unwrap();
    let guid = "0b3c2a8e-6f1d-4c5e-9a7b-1d2e3f405162";
    let (s, created) = create_solution(&db, guid, "Проект").await.unwrap();
    assert!(created);
    assert_eq!(s.kind, SpaceKind::Solution);
    assert_eq!(s.solution_guid.as_deref(), Some(guid));
    assert_eq!(s.name, "Проект");
    // повтор: тот же id, имя не меняется
    let (again, created) = create_solution(&db, guid, "Другое").await.unwrap();
    assert!(!created);
    assert_eq!(again, s);
    assert_eq!(get_solution(&db, guid).await.unwrap(), Some(s.clone()));
    assert_eq!(get_solution(&db, "nope").await.unwrap(), None);
    assert_eq!(get_space(&db, s.id).await.unwrap(), Some(s.clone()));
    assert_eq!(get_space(&db, Id::new()).await.unwrap(), None);
    let k = space_of_kind(&db, SpaceKind::Knowledge).await.unwrap();
    assert_eq!(k.kind, SpaceKind::Knowledge);
    assert_eq!(list_spaces(&db).await.unwrap().len(), 4);
}

/// Переход схемы v11 → v12 на базе с данными (задача 19b M3): копия `bak-v11`, индекс
/// `nodes_live_children` создан, живые и удалённые дети папки читаются как прежде.
#[tokio::test]
async fn migrate_v11_to_v12_with_trash() {
    const TS: &str = "2026-10-26T00:00:00.000000Z";
    let dd = datadir("mig-v12");
    let user = Id::new();
    let path = dd.user_db(&user);
    let (space, folder, live) = (Id::new(), Id::new(), Id::new());
    {
        let mut c = rusqlite::Connection::open(&path).unwrap();
        let v =
            zam_store::migrate::apply(&mut c, &path, &zam_store::USER_MIGRATIONS[..11]).unwrap();
        assert_eq!(v, 11);
        c.execute(
            "INSERT INTO spaces (id, kind, solution_guid, name, created_at)
             VALUES (?1, 'solution', 'g-19b', 'sol', ?2)",
            rusqlite::params![space, TS],
        )
        .unwrap();
        let ins = |id: Id, parent: Option<Id>, kind: &str, path: &str, del: Option<&str>| {
            c.execute(
                "INSERT INTO nodes (id, space_id, parent_id, kind, name, path, index_state,
                                    created_at, updated_at, deleted_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5, 'excluded', ?7, ?7, ?6)",
                rusqlite::params![id, space, parent, kind, path, del, TS],
            )
            .unwrap();
        };
        ins(folder, None, "folder", "f", None);
        ins(live, Some(folder), "note", "f/live", None);
        ins(Id::new(), Some(folder), "note", "f/gone", Some(TS));
    }
    let db = UserDbs::new(dd.users_dir()).get(user).await.unwrap();
    assert!(path.with_file_name(format!("{user}.db.bak-v11")).exists());
    let (ver, has_index): (u32, bool) = db
        .read(|c| {
            Ok((
                c.query_row("PRAGMA user_version", [], |r| r.get(0))?,
                c.query_row(
                    "SELECT EXISTS (SELECT 1 FROM sqlite_master
                                    WHERE type = 'index' AND name = 'nodes_live_children')",
                    [],
                    |r| r.get(0),
                )?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(ver, zam_store::USER_MIGRATIONS.len() as u32);
    assert!(has_index);
    let kids = zam_store::list_children(&db, space, Some(folder), 1)
        .await
        .unwrap();
    assert_eq!(kids.iter().map(|n| n.id).collect::<Vec<_>>(), vec![live]);
    let f = zam_store::get_node(&db, folder).await.unwrap();
    assert!(f.has_children);
}
