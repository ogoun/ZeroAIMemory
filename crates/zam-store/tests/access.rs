//! Реестр решений и доступ участников в `system.db` (§6.2, §11.6 ТЗ).

use std::path::PathBuf;

use zam_core::{Id, SolutionGuid};
use zam_store::{
    Access, AccessDecision, AccessState, DataDir, Db, SYSTEM_MIGRATIONS, SolutionRole, StoreError,
    SystemStore,
};

async fn store(name: &str) -> (DataDir, SystemStore) {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("access-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    let dd = DataDir::open(&dir).unwrap();
    let s = SystemStore::open(&dd).unwrap();
    (dd, s)
}

/// Владелец, участник, решение владельца.
async fn setup(name: &str) -> (DataDir, SystemStore, Id, Id, SolutionGuid) {
    let (dd, s) = store(name).await;
    let owner = s.create_user("owner", false).await.unwrap().id;
    let member = s.create_user("member", false).await.unwrap().id;
    let g = SolutionGuid::new_v7();
    s.register_solution(&g, owner, Id::new(), "проект")
        .await
        .unwrap();
    (dd, s, owner, member, g)
}

fn approve(a: Access) -> AccessDecision {
    AccessDecision::Approve(a)
}

fn reject() -> AccessDecision {
    AccessDecision::Reject {
        reason: Some("не сейчас".into()),
    }
}

#[tokio::test]
async fn register_is_idempotent_and_owner_fixed() {
    let (_dd, s) = store("register").await;
    let a = s.create_user("a", false).await.unwrap().id;
    let b = s.create_user("b", false).await.unwrap().id;
    let (g, space) = (SolutionGuid::new_v7(), Id::new());
    let (info, created) = s.register_solution(&g, a, space, "проект").await.unwrap();
    assert!(created);
    assert_eq!(
        (&info.guid, info.owner_id, info.space_id, info.name.as_str()),
        (&g, a, space, "проект")
    );
    let (again, created) = s.register_solution(&g, a, space, "другое").await.unwrap();
    assert!(!created);
    assert_eq!(again, info, "имя и владелец не меняются");
    // другой пользователь владельцем не становится
    let (other, created) = s
        .register_solution(&g, b, Id::new(), "чужое")
        .await
        .unwrap();
    assert!(!created);
    assert_eq!(other.owner_id, a);
    // пространство уже за другим GUID
    let e = s
        .register_solution(&SolutionGuid::new_v7(), a, space, "x")
        .await
        .unwrap_err();
    assert!(matches!(e, StoreError::Conflict(_)), "{e:?}");
    // нет такого пользователя
    let e = s
        .register_solution(&SolutionGuid::new_v7(), Id::new(), Id::new(), "x")
        .await
        .unwrap_err();
    assert!(matches!(e, StoreError::NotFound), "{e:?}");
    assert_eq!(s.get_solution(&g).await.unwrap(), Some(info.clone()));
    assert_eq!(
        s.solution_by_space(space).await.unwrap(),
        Some(info.clone())
    );
    assert_eq!(s.solution_by_space(Id::new()).await.unwrap(), None);
    assert_eq!(s.get_solution(&SolutionGuid::new_v7()).await.unwrap(), None);
    assert_eq!(s.list_solutions().await.unwrap(), vec![info]);
}

#[tokio::test]
async fn request_access_idempotent_pending() {
    let (_dd, s, owner, member, g) = setup("request").await;
    let (tok, _) = s
        .issue_token(member, "laptop", Access::Ro, false)
        .await
        .unwrap();
    assert_eq!(
        s.request_access(&g, member, Some(tok.id)).await.unwrap(),
        AccessState::Pending
    );
    assert_eq!(
        s.request_access(&g, member, None).await.unwrap(),
        AccessState::Pending
    );
    let list = s.list_access(&g).await.unwrap();
    assert_eq!(list.len(), 1);
    let e = &list[0];
    assert_eq!(
        (e.user_id, e.user_name.as_str(), e.state, e.access),
        (member, "member", AccessState::Pending, Access::Ro),
        "запрошенный режим — режим токена"
    );
    assert!(e.decided_at.is_none() && e.decided_by.is_none());
    // владелец участником не бывает
    let err = s.request_access(&g, owner, None).await.unwrap_err();
    assert!(matches!(err, StoreError::BadInput(_)), "{err:?}");
    // нет решения или пользователя
    let err = s
        .request_access(&SolutionGuid::new_v7(), member, None)
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::NotFound), "{err:?}");
    let err = s.request_access(&g, Id::new(), None).await.unwrap_err();
    assert!(matches!(err, StoreError::NotFound), "{err:?}");
}

#[tokio::test]
async fn roles_owner_member_pending() {
    let (_dd, s, owner, member, g) = setup("roles").await;
    let stranger = s.create_user("stranger", false).await.unwrap().id;
    assert_eq!(
        s.solution_role(&g, owner).await.unwrap(),
        Some(SolutionRole::Owner)
    );
    assert_eq!(s.solution_role(&g, stranger).await.unwrap(), None);
    assert_eq!(
        s.solution_role(&SolutionGuid::new_v7(), owner)
            .await
            .unwrap(),
        None
    );
    s.request_access(&g, member, None).await.unwrap();
    assert_eq!(
        s.solution_role(&g, member).await.unwrap(),
        Some(SolutionRole::Pending)
    );
    s.decide_access(&g, member, approve(Access::Ro), "admin")
        .await
        .unwrap();
    assert_eq!(
        s.solution_role(&g, member).await.unwrap(),
        Some(SolutionRole::Member(Access::Ro))
    );
    let e = &s.list_access(&g).await.unwrap()[0];
    assert_eq!(
        (e.state, e.access, e.decided_by.as_deref()),
        (AccessState::Approved, Access::Ro, Some("admin"))
    );
    assert!(e.decided_at.is_some());
}

/// Приводит участника к состоянию `st` (без строки — `None`).
async fn bring_to(s: &SystemStore, g: &SolutionGuid, u: Id, st: AccessState) {
    s.request_access(g, u, None).await.unwrap();
    match st {
        AccessState::Pending => {}
        AccessState::Approved => s
            .decide_access(g, u, approve(Access::Rw), "admin")
            .await
            .unwrap(),
        AccessState::Rejected => s.decide_access(g, u, reject(), "admin").await.unwrap(),
        AccessState::Revoked => {
            s.decide_access(g, u, approve(Access::Rw), "admin")
                .await
                .unwrap();
            s.revoke_access(g, u, "admin").await.unwrap();
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Act {
    Approve,
    Reject,
    Revoke,
}

#[tokio::test]
async fn transition_matrix() {
    use AccessState::*;
    // (из, действие, итог: Some(состояние) — разрешено, None — Conflict)
    let matrix = [
        (Pending, Act::Approve, Some(Approved)),
        (Pending, Act::Reject, Some(Rejected)),
        (Pending, Act::Revoke, None),
        (Approved, Act::Approve, Some(Approved)),
        (Approved, Act::Reject, None),
        (Approved, Act::Revoke, Some(Revoked)),
        (Rejected, Act::Approve, Some(Approved)),
        (Rejected, Act::Reject, None),
        (Rejected, Act::Revoke, None),
        (Revoked, Act::Approve, Some(Approved)),
        (Revoked, Act::Reject, None),
        (Revoked, Act::Revoke, None),
    ];
    let (_dd, s, owner, _, _) = setup("matrix").await;
    for (i, (from, act, to)) in matrix.into_iter().enumerate() {
        let g = SolutionGuid::new_v7();
        s.register_solution(&g, owner, Id::new(), "p")
            .await
            .unwrap();
        let u = s.create_user(&format!("u{i}"), false).await.unwrap().id;
        bring_to(&s, &g, u, from).await;
        let r = match act {
            Act::Approve => s.decide_access(&g, u, approve(Access::Ro), "admin").await,
            Act::Reject => s.decide_access(&g, u, reject(), "admin").await,
            Act::Revoke => s.revoke_access(&g, u, "admin").await,
        };
        let state = s.list_access(&g).await.unwrap()[0].state;
        match to {
            Some(want) => {
                r.unwrap_or_else(|e| panic!("{from:?} {act:?}: {e:?}"));
                assert_eq!(state, want, "{from:?} {act:?}");
            }
            None => {
                assert!(
                    matches!(r, Err(StoreError::Conflict(_))),
                    "{from:?} {act:?}: {r:?}"
                );
                assert_eq!(state, from, "{from:?} {act:?}: состояние не меняется");
            }
        }
    }
    // смена режима одобренного участника
    let g = SolutionGuid::new_v7();
    s.register_solution(&g, owner, Id::new(), "p")
        .await
        .unwrap();
    let u = s.create_user("mode", false).await.unwrap().id;
    bring_to(&s, &g, u, Approved).await;
    s.decide_access(&g, u, approve(Access::Ro), "admin")
        .await
        .unwrap();
    assert_eq!(
        s.solution_role(&g, u).await.unwrap(),
        Some(SolutionRole::Member(Access::Ro))
    );
    // нет запроса
    let stranger = s.create_user("stranger", false).await.unwrap().id;
    for r in [
        s.decide_access(&g, stranger, approve(Access::Rw), "admin")
            .await,
        s.revoke_access(&g, stranger, "admin").await,
    ] {
        assert!(matches!(r, Err(StoreError::NotFound)), "{r:?}");
    }
}

#[tokio::test]
async fn rejected_not_reset_by_new_request() {
    let (_dd, s, owner, member, g) = setup("rejected").await;
    s.request_access(&g, member, None).await.unwrap();
    s.decide_access(&g, member, reject(), "admin")
        .await
        .unwrap();
    assert_eq!(
        s.request_access(&g, member, None).await.unwrap(),
        AccessState::Rejected
    );
    assert_eq!(
        s.solution_role(&g, member).await.unwrap(),
        Some(SolutionRole::Rejected)
    );
    // отозванный — тоже
    let other = s.create_user("other", false).await.unwrap().id;
    s.request_access(&g, other, None).await.unwrap();
    s.decide_access(&g, other, approve(Access::Rw), &owner.to_string())
        .await
        .unwrap();
    s.revoke_access(&g, other, &owner.to_string())
        .await
        .unwrap();
    assert_eq!(
        s.request_access(&g, other, None).await.unwrap(),
        AccessState::Revoked
    );
    assert_eq!(
        s.solution_role(&g, other).await.unwrap(),
        Some(SolutionRole::Revoked)
    );
    let owner_s = owner.to_string();
    let e = s
        .list_access(&g)
        .await
        .unwrap()
        .into_iter()
        .find(|e| e.user_id == other)
        .unwrap();
    assert_eq!(e.decided_by.as_deref(), Some(owner_s.as_str()));
}

#[tokio::test]
async fn audit_rows_written() {
    let (_dd, s, _owner, member, g) = setup("audit").await;
    let other = s.create_user("other", false).await.unwrap().id;
    s.request_access(&g, member, None).await.unwrap();
    s.request_access(&g, member, None).await.unwrap(); // повтор — без записи
    s.decide_access(&g, member, approve(Access::Rw), "admin")
        .await
        .unwrap();
    s.decide_access(&g, member, approve(Access::Ro), "admin")
        .await
        .unwrap();
    s.revoke_access(&g, member, "admin").await.unwrap();
    s.request_access(&g, other, None).await.unwrap();
    s.decide_access(&g, other, reject(), "admin").await.unwrap();
    let mut rows = s.list_audit(100, None).await.unwrap();
    rows.reverse();
    let got: Vec<(&str, Option<Id>)> = rows
        .iter()
        .map(|r| (r.action.as_str(), r.user_id))
        .collect();
    assert_eq!(
        got,
        vec![
            ("solution.register", Some(_owner)),
            ("solution.access.request", Some(member)),
            ("solution.access.approve", Some(member)),
            ("solution.access.mode", Some(member)),
            ("solution.access.revoke", Some(member)),
            ("solution.access.request", Some(other)),
            ("solution.access.reject", Some(other)),
        ]
    );
    assert!(rows.iter().all(|r| r.target.as_deref() == Some(g.as_str())));
    assert_eq!(rows[2].actor, "admin");
    assert_eq!(rows[3].details["access"], "ro");
    assert_eq!(rows[6].details["reason"], "не сейчас");
}

#[tokio::test]
async fn accessible_solutions_only_owner_and_approved() {
    let (_dd, s) = store("accessible").await;
    let a = s.create_user("a", false).await.unwrap().id;
    let b = s.create_user("b", false).await.unwrap().id;
    let (g1, g2, g3) = (
        SolutionGuid::new_v7(),
        SolutionGuid::new_v7(),
        SolutionGuid::new_v7(),
    );
    s.register_solution(&g1, a, Id::new(), "мой").await.unwrap();
    s.register_solution(&g2, b, Id::new(), "общий")
        .await
        .unwrap();
    s.register_solution(&g3, b, Id::new(), "отозван")
        .await
        .unwrap();
    s.request_access(&g2, a, None).await.unwrap();
    s.request_access(&g3, a, None).await.unwrap();
    let names = |v: Vec<(zam_store::SolutionInfo, SolutionRole)>| {
        v.into_iter().map(|(i, r)| (i.guid, r)).collect::<Vec<_>>()
    };
    assert_eq!(
        names(s.accessible_solutions(a).await.unwrap()),
        vec![(g1.clone(), SolutionRole::Owner)]
    );
    s.decide_access(&g2, a, approve(Access::Rw), "admin")
        .await
        .unwrap();
    s.decide_access(&g3, a, approve(Access::Rw), "admin")
        .await
        .unwrap();
    s.revoke_access(&g3, a, "admin").await.unwrap();
    assert_eq!(
        names(s.accessible_solutions(a).await.unwrap()),
        vec![
            (g1.clone(), SolutionRole::Owner),
            (g2.clone(), SolutionRole::Member(Access::Rw))
        ]
    );
    assert_eq!(
        names(s.accessible_solutions(b).await.unwrap()),
        vec![(g2, SolutionRole::Owner), (g3, SolutionRole::Owner)]
    );
}

#[tokio::test]
async fn pending_count_for_owner() {
    let (_dd, s, owner, member, g) = setup("pending").await;
    let other = s.create_user("other", false).await.unwrap().id;
    let g2 = SolutionGuid::new_v7();
    s.register_solution(&g2, owner, Id::new(), "второй")
        .await
        .unwrap();
    assert_eq!(s.pending_requests_for_owner(owner).await.unwrap(), 0);
    s.request_access(&g, member, None).await.unwrap();
    s.request_access(&g, other, None).await.unwrap();
    s.request_access(&g2, member, None).await.unwrap();
    assert_eq!(s.pending_requests_for_owner(owner).await.unwrap(), 3);
    assert_eq!(s.pending_requests_for_owner(member).await.unwrap(), 0);
    s.decide_access(&g, member, approve(Access::Rw), "admin")
        .await
        .unwrap();
    s.decide_access(&g, other, reject(), "admin").await.unwrap();
    assert_eq!(s.pending_requests_for_owner(owner).await.unwrap(), 1);
}

#[tokio::test]
async fn migration_v2_to_v3_keeps_data() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("access-migv2");
    let _ = std::fs::remove_dir_all(&dir);
    let dd = DataDir::open(&dir).unwrap();
    let user = Id::new();
    {
        let db = Db::open(&dd.system_db(), &SYSTEM_MIGRATIONS[..2]).unwrap();
        db.write(move |tx| {
            tx.execute(
                "INSERT INTO users (id, name, created_at) VALUES (?1, 'ivan', ?2)",
                rusqlite::params![user, "2026-10-18T00:00:00.000000Z"],
            )?;
            tx.execute(
                "INSERT INTO index_skipped (user_id, error, attempts, skipped_at)
                 VALUES (?1, 'e', 1, '2026-10-18T00:00:00.000000Z')",
                [user],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    }
    let s = SystemStore::open(&dd).unwrap();
    assert!(dd.system_db().with_file_name("system.db.bak-v2").exists());
    let u = s.get_user(user).await.unwrap().unwrap();
    assert_eq!(u.name, "ivan");
    assert_eq!(s.index_skipped().await.unwrap().len(), 1);
    let ver: u32 = rusqlite::Connection::open(dd.system_db())
        .unwrap()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ver, SYSTEM_MIGRATIONS.len() as u32);
    // новые таблицы работают
    let g = SolutionGuid::new_v7();
    s.register_solution(&g, user, Id::new(), "p").await.unwrap();
    assert_eq!(s.list_solutions().await.unwrap().len(), 1);
}

#[tokio::test]
async fn member_solutions_all_states_without_own() {
    let (_dd, s, owner, member, g) = setup("member-solutions").await;
    assert!(s.member_solutions(member).await.unwrap().is_empty());
    s.request_access(&g, member, None).await.unwrap();
    let l = s.member_solutions(member).await.unwrap();
    assert_eq!(l.len(), 1);
    assert_eq!((&l[0].0.guid, l[0].1), (&g, SolutionRole::Pending));
    for (st, role) in [
        (AccessState::Approved, SolutionRole::Member(Access::Rw)),
        (AccessState::Revoked, SolutionRole::Revoked),
    ] {
        bring_to(&s, &g, member, st).await;
        assert_eq!(s.member_solutions(member).await.unwrap()[0].1, role);
    }
    // свои решения сюда не входят
    assert!(s.member_solutions(owner).await.unwrap().is_empty());
}
