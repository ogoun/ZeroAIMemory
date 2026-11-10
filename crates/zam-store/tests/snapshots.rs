//! Снимок сессии (`session_snapshots`, спецификация M6 §8а, миграция 0025): запись с
//! перезаписью, чтение, срок хранения и удаление вместе с сессией.
use std::path::PathBuf;

use jiff::{SignedDuration, Timestamp};
use zam_core::Id;
use zam_core::snapshot::{SnapCommand, SnapStatus, SnapTrigger, SnapshotData};
use zam_store::{
    DataDir, Db, SessionTouch, SnapshotRow, StoreError, UserDbs, digest_snapshot, end_session,
    gc_sessions, gc_snapshots, get_session, get_snapshot, put_snapshot, touch_session,
};

async fn db(name: &str) -> (DataDir, Db) {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("snapshots-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    let dd = DataDir::open(&dir).unwrap();
    let db = UserDbs::new(dd.users_dir()).get(Id::new()).await.unwrap();
    (dd, db)
}

async fn session(db: &Db, id: &str) {
    let s = SessionTouch {
        id: id.into(),
        ..SessionTouch::default()
    };
    touch_session(db, &s).await.unwrap();
}

/// Время с точностью хранения (микросекунды).
fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

fn row(session: &str, taken_at: &str, trigger: SnapTrigger) -> SnapshotRow {
    SnapshotRow {
        session_id: session.into(),
        taken_at: ts(taken_at),
        trigger,
        agent: Some("claude".into()),
        status: SnapStatus::Complete,
        skipped: 0,
        bytes: 120,
        data: SnapshotData {
            compact_summary: Some("## Current Work\nМиграция 0025.".into()),
            prompts: vec!["сделай миграцию".into()],
            files: vec!["crates/zam-store/src/sessions.rs".into()],
            commands: vec![
                SnapCommand {
                    cmd: "cargo test".into(),
                    failed: Some(true),
                },
                SnapCommand {
                    cmd: "git status".into(),
                    failed: None,
                },
            ],
        },
    }
}

/// Снимок сессии один: повторная запись заменяет прежний целиком; чтение возвращает то, что
/// записано; снимка нет — `None`; сессии нет — `NotFound`, ничего не записано.
#[tokio::test]
async fn snapshot_put_overwrites() {
    let (_dd, db) = db("put").await;
    session(&db, "s1").await;
    assert_eq!(get_snapshot(&db, "s1").await.unwrap(), None);
    let first = row("s1", "2026-11-05T10:00:00.123456Z", SnapTrigger::Stop);
    put_snapshot(&db, &first).await.unwrap();
    assert_eq!(get_snapshot(&db, "s1").await.unwrap(), Some(first));
    let second = SnapshotRow {
        trigger: SnapTrigger::PostCompact,
        agent: None,
        status: SnapStatus::Partial,
        skipped: 3,
        bytes: 40,
        data: SnapshotData {
            prompts: vec!["дальше".into()],
            ..SnapshotData::default()
        },
        ..row("s1", "2026-11-05T11:00:00.000001Z", SnapTrigger::Stop)
    };
    assert!(put_snapshot(&db, &second).await.unwrap());
    assert_eq!(get_snapshot(&db, "s1").await.unwrap(), Some(second.clone()));
    // снимок старше хранимого (опоздавшая очередь клиента) новый не затирает
    let older = row("s1", "2026-11-05T10:59:59.999999Z", SnapTrigger::Stop);
    assert!(!put_snapshot(&db, &older).await.unwrap());
    assert_eq!(get_snapshot(&db, "s1").await.unwrap(), Some(second));
    let n: i64 = db
        .read(|c| Ok(c.query_row("SELECT count(*) FROM session_snapshots", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(n, 1);
    // `data` — JSON нейтрального снимка (§8а): без `compact_summary`, `failed` — только где есть
    let data: String = db
        .read(|c| Ok(c.query_row("SELECT data FROM session_snapshots", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(data, r#"{"prompts":["дальше"],"files":[],"commands":[]}"#);
    let r = put_snapshot(
        &db,
        &row("нет", "2026-11-05T10:00:00Z", SnapTrigger::Manual),
    )
    .await;
    assert!(matches!(r, Err(StoreError::NotFound)), "{r:?}");
    assert_eq!(get_snapshot(&db, "нет").await.unwrap(), None);
    assert!(get_session(&db, "нет").await.unwrap().is_none());
}

/// Срок хранения снимков (`snapshot.keep_days`): удаляются снятые раньше `cutoff`, сессии
/// остаются.
#[tokio::test]
async fn gc_snapshots_by_cutoff() {
    let (_dd, db) = db("gc").await;
    session(&db, "old").await;
    session(&db, "new").await;
    put_snapshot(&db, &row("old", "2026-10-01T00:00:00Z", SnapTrigger::Stop))
        .await
        .unwrap();
    put_snapshot(
        &db,
        &row("new", "2026-11-01T00:00:00Z", SnapTrigger::SessionEnd),
    )
    .await
    .unwrap();
    assert_eq!(
        gc_snapshots(&db, ts("2026-09-01T00:00:00Z")).await.unwrap(),
        0
    );
    assert_eq!(
        gc_snapshots(&db, ts("2026-10-15T00:00:00Z")).await.unwrap(),
        1
    );
    assert_eq!(get_snapshot(&db, "old").await.unwrap(), None);
    assert!(get_snapshot(&db, "new").await.unwrap().is_some());
    assert!(get_session(&db, "old").await.unwrap().is_some());
}

/// `gc_sessions` удаляет снимок вместе с сессией (каскада по внешнему ключу нет, таблица — в
/// явном списке §8а); снимок свежей сессии остаётся.
#[tokio::test]
async fn gc_sessions_drops_snapshots() {
    let (_dd, db) = db("gc-sessions").await;
    session(&db, "old").await;
    session(&db, "fresh").await;
    for s in ["old", "fresh"] {
        put_snapshot(&db, &row(s, "2026-11-05T00:00:00Z", SnapTrigger::Stop))
            .await
            .unwrap();
    }
    let later = Timestamp::now() + SignedDuration::from_hours(24 * 31);
    db.write(move |t| {
        t.execute(
            "UPDATE sessions SET last_seen_at = ?1 WHERE id = 'fresh'",
            [zam_store::time::fmt_ts(later)],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let cutoff = later - SignedDuration::from_hours(24 * 30);
    assert_eq!(gc_sessions(&db, cutoff).await.unwrap(), 1);
    assert_eq!(get_snapshot(&db, "old").await.unwrap(), None);
    assert!(get_session(&db, "old").await.unwrap().is_none());
    assert!(get_snapshot(&db, "fresh").await.unwrap().is_some());
}

/// §8а (решение владельца 2026-11-10): в дайджест идёт только снимок незавершённой сессии.
/// «Оборванная A, затем завершённая B» — завершённые не выбираются в самом запросе, остаётся
/// снимок A, а не пусто. Завершена — `ended_at` (E: отметка снята — снимок выбирается) или снимок
/// повода `session_end` без `ended_at` (D: следующий снимок другого повода — выбирается). Своя
/// сессия (`except`), другое решение и снимки раньше `since` не выбираются.
#[tokio::test]
async fn digest_snapshot_skips_ended_sessions() {
    let (_dd, db) = db("digest-ended").await;
    let guid = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e17";
    for id in ["A", "B", "C", "D", "E"] {
        let s = SessionTouch {
            id: id.into(),
            solution_guid: Some(guid.parse().unwrap()),
            ..SessionTouch::default()
        };
        touch_session(&db, &s).await.unwrap();
    }
    session(&db, "other").await;
    for (id, at, trigger) in [
        ("A", "2026-11-05T09:00:00Z", SnapTrigger::Stop),
        ("B", "2026-11-05T10:00:00Z", SnapTrigger::SessionEnd),
        ("D", "2026-11-05T10:30:00Z", SnapTrigger::SessionEnd),
        ("E", "2026-11-05T10:45:00Z", SnapTrigger::Stop),
        ("other", "2026-11-05T11:00:00Z", SnapTrigger::Stop),
    ] {
        put_snapshot(&db, &row(id, at, trigger)).await.unwrap();
    }
    for id in ["B", "E"] {
        assert!(end_session(&db, id, true).await.unwrap());
    }
    let since = ts("2026-11-04T00:00:00Z");
    let pick = |except: Option<&'static str>| {
        let db = db.clone();
        async move {
            digest_snapshot(&db, guid, except, since)
                .await
                .unwrap()
                .map(|r| r.session_id)
        }
    };
    // B и E завершены (`ended_at`), D — снимок `session_end`: остаётся оборванная A
    assert_eq!(pick(Some("C")).await.as_deref(), Some("A"));
    assert_eq!(pick(None).await.as_deref(), Some("A"));
    assert_eq!(pick(Some("A")).await, None);
    // окно: снимки раньше `since` не выбираются
    let late = ts("2026-11-05T09:30:00Z");
    assert_eq!(digest_snapshot(&db, guid, None, late).await.unwrap(), None);
    // отметка конца снята (сессию продолжили): E — обычная незавершённая, её снимок последний
    assert!(end_session(&db, "E", false).await.unwrap());
    assert_eq!(pick(Some("C")).await.as_deref(), Some("E"));
    // у B отметка снята, но последний снимок — `session_end`: по-прежнему завершённая
    assert!(end_session(&db, "B", false).await.unwrap());
    assert_eq!(pick(Some("E")).await.as_deref(), Some("A"));
    // D сняла снимок другого повода (работа продолжилась и оборвалась) — выбирается
    put_snapshot(&db, &row("D", "2026-11-05T11:30:00Z", SnapTrigger::Stop))
        .await
        .unwrap();
    assert_eq!(pick(Some("C")).await.as_deref(), Some("D"));
}
