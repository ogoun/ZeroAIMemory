use std::path::PathBuf;

use zam_core::{Id, hash_token, mask_token};
use zam_store::{Access, DataDir, StoreError, SystemStore};

async fn store(name: &str) -> (DataDir, SystemStore) {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("system-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    let dd = DataDir::open(&dir).unwrap();
    let s = SystemStore::open(&dd).unwrap();
    (dd, s)
}

fn raw(dd: &DataDir) -> rusqlite::Connection {
    rusqlite::Connection::open(dd.system_db()).unwrap()
}

#[tokio::test]
async fn issue_and_verify_token() {
    let (dd, s) = store("issue").await;
    let u = s.create_user("ivan", false).await.unwrap();
    let (info, plain) = s.issue_token(u.id, "laptop", Access::Rw).await.unwrap();
    let a = s
        .verify_token(&plain, Some("10.0.0.5"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (a.user_id, a.access, a.token_id),
        (u.id, Access::Rw, info.id)
    );
    let stored: String = raw(&dd)
        .query_row("SELECT secret_hash FROM tokens", [], |r| r.get(0))
        .unwrap();
    assert_eq!(stored, hash_token(&plain));
    assert!(!stored.contains(&plain));
    assert_eq!(info.prefix.len(), 8);
    assert!(s.verify_token("zam_wrong", None).await.unwrap().is_none());
    let t = &s.list_tokens(u.id).await.unwrap()[0];
    assert_eq!(t.last_ip.as_deref(), Some("10.0.0.5"));
    assert!(t.last_used_at.is_some());
}

#[tokio::test]
async fn last_used_throttled_to_once_per_minute() {
    let (dd, s) = store("throttle").await;
    let u = s.create_user("ivan", false).await.unwrap();
    let (_, plain) = s.issue_token(u.id, "t", Access::Ro).await.unwrap();
    s.verify_token(&plain, Some("1.1.1.1"))
        .await
        .unwrap()
        .unwrap();
    s.verify_token(&plain, Some("2.2.2.2"))
        .await
        .unwrap()
        .unwrap();
    let t = &s.list_tokens(u.id).await.unwrap()[0];
    assert_eq!(t.last_ip.as_deref(), Some("1.1.1.1"));
    // состарить last_used_at: обновление снова разрешено
    raw(&dd)
        .execute("UPDATE tokens SET last_used_at='2000-01-01T00:00:00Z'", [])
        .unwrap();
    s.verify_token(&plain, Some("3.3.3.3"))
        .await
        .unwrap()
        .unwrap();
    let t = &s.list_tokens(u.id).await.unwrap()[0];
    assert_eq!(t.last_ip.as_deref(), Some("3.3.3.3"));
}

#[tokio::test]
async fn revoked_token_rejected_immediately() {
    let (_dd, s) = store("revoke").await;
    let u = s.create_user("ivan", false).await.unwrap();
    let (info, plain) = s.issue_token(u.id, "t", Access::Rw).await.unwrap();
    assert!(s.verify_token(&plain, None).await.unwrap().is_some());
    s.revoke_token(info.id).await.unwrap();
    assert!(s.verify_token(&plain, None).await.unwrap().is_none());
    assert!(s.list_tokens(u.id).await.unwrap()[0].revoked_at.is_some());
}

#[tokio::test]
async fn disabled_user_token_rejected() {
    let (_dd, s) = store("disabled").await;
    let u = s.create_user("ivan", false).await.unwrap();
    let (_, plain) = s.issue_token(u.id, "t", Access::Rw).await.unwrap();
    s.set_user_disabled(u.id, true).await.unwrap();
    assert!(s.verify_token(&plain, None).await.unwrap().is_none());
    assert!(
        s.get_user(u.id)
            .await
            .unwrap()
            .unwrap()
            .disabled_at
            .is_some()
    );
    s.set_user_disabled(u.id, false).await.unwrap();
    assert!(s.verify_token(&plain, None).await.unwrap().is_some());
}

#[tokio::test]
async fn users_conflict_notfound_and_admin() {
    let (_dd, s) = store("users").await;
    assert!(s.admin_user().await.unwrap().is_none());
    let a = s.create_user("root", true).await.unwrap();
    s.create_user("ivan", false).await.unwrap();
    let err = s.create_user("ivan", false).await.unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)), "{err:?}");
    assert_eq!(s.list_users().await.unwrap().len(), 2);
    assert_eq!(s.admin_user().await.unwrap().unwrap().id, a.id);
    let err = s.issue_token(Id::new(), "t", Access::Rw).await.unwrap_err();
    assert!(matches!(err, StoreError::NotFound), "{err:?}");
    assert!(s.get_user(Id::new()).await.unwrap().is_none());
}

#[tokio::test]
async fn web_session_lifecycle() {
    let (dd, s) = store("sessions").await;
    let (id, csrf) = s.create_web_session(Some("10.0.0.1")).await.unwrap();
    assert!(id.len() >= 43 && csrf.len() >= 43);
    let ws = s.get_web_session(&id).await.unwrap().unwrap();
    assert_eq!(ws.csrf_token, csrf);
    assert!(ws.expires_at > ws.created_at);
    // в таблице нет открытого id
    let stored: String = raw(&dd)
        .query_row("SELECT id FROM web_sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(stored, hash_token(&id));
    s.delete_web_session(&id).await.unwrap();
    assert!(s.get_web_session(&id).await.unwrap().is_none());
}

#[tokio::test]
async fn expired_session_not_returned_and_removed() {
    let (dd, s) = store("expired").await;
    let (id, _) = s.create_web_session(None).await.unwrap();
    raw(&dd)
        .execute(
            "UPDATE web_sessions SET expires_at='2000-01-01T00:00:00Z'",
            [],
        )
        .unwrap();
    assert!(s.get_web_session(&id).await.unwrap().is_none());
    let n: i64 = raw(&dd)
        .query_row("SELECT count(*) FROM web_sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);
}

#[tokio::test]
async fn audit_has_no_secrets() {
    let (dd, s) = store("audit").await;
    let u = s.create_user("ivan", false).await.unwrap();
    let (info, plain) = s.issue_token(u.id, "t", Access::Rw).await.unwrap();
    s.audit(
        &info.id.to_string(),
        Some(u.id),
        "token.use",
        Some("x"),
        Some("10.0.0.1"),
        serde_json::json!({"token": mask_token(&plain)}),
    )
    .await
    .unwrap();
    let conn = raw(&dd);
    let mut st = conn
        .prepare("SELECT id, ts, actor, user_id, action, target, ip, details FROM audit")
        .unwrap();
    let rows: Vec<Vec<String>> = st
        .query_map([], |r| {
            (0..8)
                .map(|i| {
                    r.get::<_, rusqlite::types::Value>(i)
                        .map(|v| format!("{v:?}"))
                })
                .collect()
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(rows.len(), 1);
    assert!(rows.iter().flatten().all(|c| !c.contains(&plain)));
    let l = s.list_audit(10, None).await.unwrap();
    assert_eq!(l[0].details["token"], mask_token(&plain));
    assert_eq!(l[0].user_id, Some(u.id));
}

#[tokio::test]
async fn audit_pagination_newest_first() {
    let (_dd, s) = store("auditpage").await;
    for i in 0..5 {
        s.audit(
            "admin",
            None,
            &format!("a{i}"),
            None,
            None,
            serde_json::json!({}),
        )
        .await
        .unwrap();
    }
    let p1 = s.list_audit(2, None).await.unwrap();
    let a1: Vec<_> = p1.iter().map(|r| r.action.as_str()).collect();
    assert_eq!(a1, ["a4", "a3"]);
    let p2 = s.list_audit(10, Some(p1[1].id)).await.unwrap();
    let a2: Vec<_> = p2.iter().map(|r| r.action.as_str()).collect();
    assert_eq!(a2, ["a2", "a1", "a0"]);
}

#[tokio::test]
async fn settings_roundtrip() {
    let (_dd, s) = store("settings").await;
    assert_eq!(s.get_setting::<Vec<u32>>("k").await.unwrap(), None);
    s.set_setting("k", &vec![1u32, 2]).await.unwrap();
    assert_eq!(
        s.get_setting::<Vec<u32>>("k").await.unwrap(),
        Some(vec![1, 2])
    );
    s.set_setting("k", &"v").await.unwrap();
    assert_eq!(
        s.get_setting::<String>("k").await.unwrap().as_deref(),
        Some("v")
    );
}
