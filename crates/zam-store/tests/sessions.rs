//! Сессии агентов, выданное сессии (`session_seen`) и показы/оценки (`usage`) — §6.3, §9.3,
//! §10.5 (задача 5 M4).
use std::path::PathBuf;

use jiff::{SignedDuration, Timestamp};
use zam_core::{Id, TargetKind, TargetRef};
use zam_store::{
    DataDir, SeenItem, SeenKey, SessionTouch, UsageEvent, UsageKind, UsageSource, UserDbs,
    WHOLE_TARGET, add_memory_write, end_session, gc_sessions, get_session, latest_session,
    list_sessions, mark_seen, record_usage, seen_revs, touch_session, usage_counts,
};

fn datadir(name: &str) -> DataDir {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("sessions-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    DataDir::open(&dir).unwrap()
}

async fn db(name: &str) -> (DataDir, zam_store::Db) {
    let dd = datadir(name);
    let db = UserDbs::new(dd.users_dir()).get(Id::new()).await.unwrap();
    (dd, db)
}

fn touch(id: &str) -> SessionTouch {
    SessionTouch {
        id: id.into(),
        ..SessionTouch::default()
    }
}

fn node(id: Id) -> TargetRef {
    TargetRef {
        kind: TargetKind::Node,
        id,
    }
}

/// Первое касание создаёт сессию, следующие обновляют `last_seen_at` и дописывают известные
/// поля, не стирая прежние; запись в память увеличивает `memory_writes` и тоже создаёт сессию.
#[tokio::test]
async fn touch_session_upsert() {
    let (_dd, db) = db("upsert").await;
    let tok = Id::new();
    touch_session(
        &db,
        &SessionTouch {
            id: "s1".into(),
            token_id: Some(tok),
            agent: Some("claude-code".into()),
            machine: Some("box".into()),
            ..SessionTouch::default()
        },
    )
    .await
    .unwrap();
    let first = get_session(&db, "s1").await.unwrap().unwrap();
    assert_eq!(first.started_at, first.last_seen_at);
    assert_eq!(first.memory_writes, 0);
    assert_eq!(first.ended_at, None);
    touch_session(
        &db,
        &SessionTouch {
            id: "s1".into(),
            area: Some("services/parser".into()),
            ..SessionTouch::default()
        },
    )
    .await
    .unwrap();
    let s = get_session(&db, "s1").await.unwrap().unwrap();
    assert_eq!(s.started_at, first.started_at);
    assert!(s.last_seen_at >= first.last_seen_at);
    assert_eq!(s.token_id, Some(tok));
    assert_eq!(s.agent.as_deref(), Some("claude-code"));
    assert_eq!(s.machine.as_deref(), Some("box"));
    assert_eq!(s.area.as_deref(), Some("services/parser"));

    add_memory_write(&db, &touch("s1")).await.unwrap();
    add_memory_write(&db, &touch("s1")).await.unwrap();
    assert_eq!(
        get_session(&db, "s1").await.unwrap().unwrap().memory_writes,
        2
    );
    // сессии ещё нет — запись в память создаёт её с счётчиком 1
    add_memory_write(&db, &touch("s2")).await.unwrap();
    assert_eq!(
        get_session(&db, "s2").await.unwrap().unwrap().memory_writes,
        1
    );

    assert!(end_session(&db, "s1", true).await.unwrap());
    assert!(
        get_session(&db, "s1")
            .await
            .unwrap()
            .unwrap()
            .ended_at
            .is_some()
    );
    assert!(!end_session(&db, "нет", true).await.unwrap());
    assert_eq!(get_session(&db, "нет").await.unwrap(), None);

    // свежие — первыми (конец сессии — тоже обращение к ней)
    let ids: Vec<String> = list_sessions(&db, None, 10)
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(ids, ["s1", "s2"]);
    let g: zam_core::SolutionGuid = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e51".parse().unwrap();
    touch_session(
        &db,
        &SessionTouch {
            id: "s3".into(),
            solution_guid: Some(g.clone()),
            ..SessionTouch::default()
        },
    )
    .await
    .unwrap();
    let only: Vec<String> = list_sessions(&db, Some(&g), 10)
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(only, ["s3"]);
    assert_eq!(list_sessions(&db, None, 2).await.unwrap().len(), 2);

    // пустой и слишком длинный id — отказ
    for bad in [String::new(), "я".repeat(129)] {
        assert!(touch_session(&db, &touch(&bad)).await.is_err());
    }
}

/// `mark_seen` сохраняет большую ревизию части цели; части (чанк, цель целиком), сессии и
/// владельцы баз различаются (решение контроллера R11).
#[tokio::test]
async fn mark_seen_keeps_latest_rev() {
    let (_dd, db) = db("seen").await;
    let (owner, other_owner, n) = (Id::new(), Id::new(), Id::new());
    let key = |owner_id, target, part: &str| SeenKey {
        owner_id,
        target,
        part: part.into(),
    };
    let chunk = key(owner, node(n), "c1");
    let item = |k: &SeenKey, rev| SeenItem {
        key: k.clone(),
        rev,
    };
    // сессии нет — создаётся (вместе с полями касания)
    let s = SessionTouch {
        id: "a".into(),
        machine: Some("box".into()),
        ..SessionTouch::default()
    };
    mark_seen(&db, &s, &[item(&chunk, 3)]).await.unwrap();
    let got = get_session(&db, "a").await.unwrap().unwrap();
    assert_eq!(got.machine.as_deref(), Some("box"));
    mark_seen(&db, &s, &[item(&chunk, 5)]).await.unwrap();
    // старая ревизия позже не откатывает отметку
    mark_seen(&db, &s, &[item(&chunk, 4)]).await.unwrap();
    // пустой список только касается сессии
    mark_seen(&db, &touch("пустая"), &[]).await.unwrap();
    assert!(get_session(&db, "пустая").await.unwrap().is_some());
    let other_chunk = key(owner, node(n), "c2");
    let whole = key(owner, node(n), WHOLE_TARGET);
    let foreign = key(other_owner, node(n), "c1");
    let entry = key(
        owner,
        TargetRef {
            kind: TargetKind::Entry,
            id: n,
        },
        "c1",
    );
    let all = [
        chunk.clone(),
        other_chunk.clone(),
        whole.clone(),
        foreign,
        entry,
    ];
    let got = seen_revs(&db, "a", &all).await.unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[&chunk], 5);
    // цель целиком — своя часть
    mark_seen(&db, &s, &[item(&whole, 6)]).await.unwrap();
    let got = seen_revs(&db, "a", &all).await.unwrap();
    assert_eq!((got[&chunk], got[&whole], got.len()), (5, 6, 2));
    assert!(seen_revs(&db, "b", &[chunk]).await.unwrap().is_empty());
    assert!(seen_revs(&db, "a", &[]).await.unwrap().is_empty());
}

/// Счётчики по событиям одной цели; события других целей не смешиваются.
#[tokio::test]
async fn usage_counts_by_event() {
    let (_dd, db) = db("usage").await;
    let (user, e1, e2) = (Id::new(), Id::new(), Id::new());
    let t1 = TargetRef {
        kind: TargetKind::Entry,
        id: e1,
    };
    let ev = |target, event, source| UsageEvent {
        user_id: user,
        session_id: Some("s".into()),
        target,
        event,
        source: Some(source),
    };
    for (event, source) in [
        (UsageKind::RecallShown, UsageSource::PreTool),
        (UsageKind::RecallShown, UsageSource::SessionStart),
        (UsageKind::RecallShown, UsageSource::Prompt),
        (UsageKind::Useful, UsageSource::Agent),
        (UsageKind::Useless, UsageSource::Webui),
        (UsageKind::Useless, UsageSource::Agent),
    ] {
        record_usage(&db, &ev(t1, event, source)).await.unwrap();
    }
    let t2 = TargetRef {
        kind: TargetKind::Entry,
        id: e2,
    };
    record_usage(&db, &ev(t2, UsageKind::Useful, UsageSource::Agent))
        .await
        .unwrap();
    let c = usage_counts(&db, t1).await.unwrap();
    assert_eq!((c.shown, c.useful, c.useless), (3, 1, 2));
    let c = usage_counts(&db, t2).await.unwrap();
    assert_eq!((c.shown, c.useful, c.useless), (0, 1, 0));
    let none = usage_counts(&db, node(Id::new())).await.unwrap();
    assert_eq!((none.shown, none.useful, none.useless), (0, 0, 0));
}

/// Сессии без обращений дольше срока удаляются вместе с выданным им; свежие остаются.
#[tokio::test]
async fn gc_sessions_by_last_seen() {
    let (_dd, db) = db("gc").await;
    let key = SeenKey {
        owner_id: Id::new(),
        target: node(Id::new()),
        part: WHOLE_TARGET.into(),
    };
    let item = SeenItem {
        key: key.clone(),
        rev: 1,
    };
    mark_seen(&db, &touch("old"), &[item]).await.unwrap();
    touch_session(&db, &touch("fresh")).await.unwrap();
    let ttl = SignedDuration::from_hours(24 * 30);
    // через 29 дней никто не удаляется
    let soon = Timestamp::now() + SignedDuration::from_hours(24 * 29);
    assert_eq!(gc_sessions(&db, soon - ttl).await.unwrap(), 0);
    // «fresh» трогают позже — через 31 день от прежнего касания уходит только «old»
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
    assert_eq!(gc_sessions(&db, later - ttl).await.unwrap(), 1);
    assert_eq!(get_session(&db, "old").await.unwrap(), None);
    assert!(get_session(&db, "fresh").await.unwrap().is_some());
    assert!(seen_revs(&db, "old", &[key]).await.unwrap().is_empty());
}

/// Переход схемы v12 → v13 на базе с данными: копия `bak-v12`, новые таблицы пусты и
/// работают, прежние данные на месте.
#[tokio::test]
async fn migrate_v12_to_v13() {
    const TS: &str = "2026-10-27T00:00:00.000000Z";
    let dd = datadir("mig-v13");
    let user = Id::new();
    let path = dd.user_db(&user);
    let space = Id::new();
    {
        let mut c = rusqlite::Connection::open(&path).unwrap();
        let v =
            zam_store::migrate::apply(&mut c, &path, &zam_store::USER_MIGRATIONS[..12]).unwrap();
        assert_eq!(v, 12);
        c.execute(
            "INSERT INTO spaces (id, kind, solution_guid, name, created_at)
             VALUES (?1, 'solution', 'g-m4-5', 'sol', ?2)",
            rusqlite::params![space, TS],
        )
        .unwrap();
    }
    let db = UserDbs::new(dd.users_dir()).get(user).await.unwrap();
    assert!(path.with_file_name(format!("{user}.db.bak-v12")).exists());
    let (ver, tables): (u32, i64) = db
        .read(|c| {
            Ok((
                c.query_row("PRAGMA user_version", [], |r| r.get(0))?,
                c.query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type = 'table'
                     AND name IN ('sessions', 'session_seen', 'usage')",
                    [],
                    |r| r.get(0),
                )?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(ver, zam_store::USER_MIGRATIONS.len() as u32);
    assert_eq!(tables, 3);
    assert!(zam_store::get_space(&db, space).await.unwrap().is_some());
    assert!(list_sessions(&db, None, 10).await.unwrap().is_empty());
    touch_session(&db, &touch("после-миграции")).await.unwrap();
    assert_eq!(list_sessions(&db, None, 10).await.unwrap().len(), 1);
}

/// Последняя незавершённая сессия с обращением не раньше срока; с машиной — только этой
/// машины (R13: MCP без `X-Zam-Session`, задача 10 M4).
#[tokio::test]
async fn latest_session_by_machine_and_age() {
    let (_dd, db) = db("latest").await;
    let since = || Timestamp::now() - SignedDuration::from_hours(12);
    assert_eq!(
        latest_session(&db, None, None, since()).await.unwrap(),
        None
    );
    let on = |id: &str, m: &str| SessionTouch {
        id: id.into(),
        machine: Some(m.into()),
        ..SessionTouch::default()
    };
    touch_session(&db, &on("a", "box-1")).await.unwrap();
    touch_session(&db, &on("b", "box-2")).await.unwrap();
    let id = |s: Option<zam_store::Session>| s.map(|s| s.id);
    assert_eq!(
        id(latest_session(&db, None, None, since()).await.unwrap()).as_deref(),
        Some("b")
    );
    assert_eq!(
        id(latest_session(&db, Some("box-1"), None, since())
            .await
            .unwrap())
        .as_deref(),
        Some("a")
    );
    assert_eq!(
        latest_session(&db, Some("box-3"), None, since())
            .await
            .unwrap(),
        None
    );
    // с решением — только сессии этого решения
    let g1: zam_core::SolutionGuid = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e71".parse().unwrap();
    let g2: zam_core::SolutionGuid = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e72".parse().unwrap();
    touch_session(
        &db,
        &SessionTouch {
            solution_guid: Some(g1.clone()),
            ..on("a", "box-1")
        },
    )
    .await
    .unwrap();
    assert_eq!(
        id(latest_session(&db, None, Some(&g1), since()).await.unwrap()).as_deref(),
        Some("a")
    );
    assert_eq!(
        latest_session(&db, None, Some(&g2), since()).await.unwrap(),
        None
    );
    assert_eq!(
        id(latest_session(&db, None, None, since()).await.unwrap()).as_deref(),
        Some("a")
    );
    touch_session(&db, &on("b", "box-2")).await.unwrap();
    // завершённая сессия не подходит
    assert!(end_session(&db, "b", true).await.unwrap());
    assert_eq!(
        id(latest_session(&db, None, None, since()).await.unwrap()).as_deref(),
        Some("a")
    );
    // старше срока — тоже
    let old = Timestamp::now() - SignedDuration::from_hours(13);
    db.write(move |t| {
        t.execute(
            "UPDATE sessions SET last_seen_at = ?1 WHERE id = 'a'",
            [zam_store::time::fmt_ts(old)],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
        latest_session(&db, None, None, since()).await.unwrap(),
        None
    );
}

/// Сессия субагента (M4.5 §9.4, миграция 0017): `parent_id` и `agent_type` пишутся, повторное
/// касание без них их не стирает; длинный или с управляющими символами тип — ошибка ввода.
/// `session_tokens` удаляется вместе с сессией сроком хранения.
#[tokio::test]
async fn subagent_session_fields_and_tokens_gc() {
    let (_dd, db) = db("subagent").await;
    let sub = SessionTouch {
        id: "s1:a7".into(),
        parent_id: Some("s1".into()),
        agent_type: Some("Explore".into()),
        ..SessionTouch::default()
    };
    touch_session(&db, &sub).await.unwrap();
    touch_session(&db, &touch("s1:a7")).await.unwrap();
    let s = get_session(&db, "s1:a7").await.unwrap().unwrap();
    assert_eq!(s.parent_id.as_deref(), Some("s1"));
    assert_eq!(s.agent_type.as_deref(), Some("Explore"));
    let s = serde_json::to_value(&s).unwrap();
    assert_eq!(s["parent_id"], "s1");
    assert_eq!(s["agent_type"], "Explore");
    for bad in ["x".repeat(129), "a\u{7}b".to_string()] {
        let t = SessionTouch {
            agent_type: Some(bad),
            ..touch("s2")
        };
        assert!(touch_session(&db, &t).await.is_err());
    }
    db.write(|t| {
        t.execute(
            "INSERT INTO session_tokens (session_id, event, calls, tokens)
             VALUES ('s1:a7', 'brief', 1, 120)",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let later = Timestamp::now() + SignedDuration::from_hours(1);
    assert_eq!(gc_sessions(&db, later).await.unwrap(), 1);
    let n: i64 = db
        .read(|c| Ok(c.query_row("SELECT count(*) FROM session_tokens", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(n, 0);
}

/// Учёт выданного (M4.5 §9.5): `add_tokens` — одна выдача, `add_token_counts` — пачка, оба
/// прибавляются к прежнему и создают сессию; `mark_seen_with_tokens` пишет показ и учёт одной
/// транзакцией; некорректное имя события — ошибка ввода, ничего не записано.
#[tokio::test]
async fn session_tokens_accumulate() {
    let (_dd, db) = db("tokens").await;
    let s = touch("s-t");
    zam_store::add_tokens(&db, &s, "prompt", 30).await.unwrap();
    zam_store::add_tokens(&db, &s, "prompt", 12).await.unwrap();
    let c = |e: &str, calls, tokens| zam_store::TokenCount {
        event: e.into(),
        calls,
        tokens,
    };
    zam_store::add_token_counts(&db, &s, &[c("pre_tool", 4, 50), c("brief", 0, 0)])
        .await
        .unwrap();
    let item = SeenItem {
        key: SeenKey {
            owner_id: Id::new(),
            target: node(Id::new()),
            part: WHOLE_TARGET.into(),
        },
        rev: 1,
    };
    zam_store::mark_seen_with_tokens(&db, &s, &[item], &[c("session_start", 1, 900)])
        .await
        .unwrap();
    for bad in ["", "Prompt", "a-b", &"x".repeat(33)] {
        assert!(
            zam_store::add_tokens(&db, &s, bad, 1).await.is_err(),
            "{bad}"
        );
    }
    let t = zam_store::session_tokens(&db, "s-t").await.unwrap();
    let got: Vec<(&str, i64, i64)> = t
        .iter()
        .map(|(e, v)| (e.as_str(), v.calls, v.tokens))
        .collect();
    // пустая прибавка (`brief` 0/0) строки не заводит
    assert_eq!(
        got,
        [
            ("pre_tool", 4, 50),
            ("prompt", 2, 42),
            ("session_start", 1, 900)
        ]
    );
    assert!(get_session(&db, "s-t").await.unwrap().is_some());
    assert!(
        zam_store::session_tokens(&db, "нет")
            .await
            .unwrap()
            .is_empty()
    );
}
