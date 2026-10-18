use std::path::PathBuf;

use zam_core::Id;
use zam_store::{DataDir, Db, StoreError};

fn tmp(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn creates_layout() {
    let d = DataDir::open(&tmp("dd1")).unwrap();
    for s in [
        "users", "blobs", "previews", "models", "qdrant", "logs", "backups",
    ] {
        assert!(d.root().join(s).is_dir(), "{s}");
    }
}

#[test]
fn second_instance_is_refused() {
    let p = tmp("dd2");
    let _a = DataDir::open(&p).unwrap();
    let err = DataDir::open(&p).unwrap_err();
    assert!(matches!(err, StoreError::DataDirLocked(_)));
    assert!(
        err.to_string()
            .contains("уже используется другим экземпляром zam-server")
    );
}

#[test]
fn lock_released_on_drop() {
    let p = tmp("dd3");
    drop(DataDir::open(&p).unwrap());
    DataDir::open(&p).unwrap();
}

#[test]
fn paths_are_under_root() {
    let d = DataDir::open(&tmp("dd4")).unwrap();
    let id = Id::new();
    assert_eq!(d.users_dir(), d.root().join("users"));
    assert_eq!(
        d.user_db(&id),
        d.root().join("users").join(format!("{id}.db"))
    );
    assert_eq!(d.system_db(), d.root().join("system.db"));
    assert_eq!(d.logs(), d.root().join("logs"));
    assert_eq!(d.qdrant(), d.root().join("qdrant"));
}

#[tokio::test]
async fn cyrillic_and_spaces_in_path() {
    let d = DataDir::open(&tmp("Мои данные/zam dir")).unwrap();
    let db = Db::open(&d.system_db(), &[]).unwrap();
    db.write(|t| {
        t.execute_batch("CREATE TABLE x(a)")?;
        Ok(())
    })
    .await
    .unwrap();
}
