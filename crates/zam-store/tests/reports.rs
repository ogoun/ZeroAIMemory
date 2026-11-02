//! Отчёты (M5.5, задача 1): вид записи `report`, миграция 0022, файлы отчёта
//! (`entry_attachments`), неизменность в хранилище и триггерах SQLite, только FTS, без рёбер и
//! `file_refs`, лента без отчётов по умолчанию и корзина.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use rusqlite::Connection;
use serde_json::json;
use zam_core::{Id, TargetKind, TargetRef};
use zam_store::{
    AnswerBy, Attribution, AuthorKind, Certainty, Channel, DataDir, Db, EdgeType, EntryFilter,
    EntryKind, EntryPatch, EntryStatus, FileRef, FtsQuery, GraphStore, IndexState, NewAnswer,
    NewEdge, NewEntry, NewReportFile, NewTrigger, QuestionError, SpaceKind, StoreError,
    TargetFilter, TaskError, TriggerKind, USER_MIGRATIONS, UserDbs, answer_question,
    attachment_link, claim_task, create_entry, create_report, create_solution, delete_entry,
    enqueue_entry_upserts, enqueue_reindex, entry_attachment, entry_attachments, entry_revisions,
    first_images, get_entry, list_entries, list_spaces, reindex_entries_page, release_task,
    restore_entry, retract_entry, search_fts, set_triggers, supersede_entry, update_entry,
};

fn datadir(name: &str) -> DataDir {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("reports-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    DataDir::open(&dir).unwrap()
}

/// База пользователя и пространство решения.
async fn setup(name: &str) -> (Db, Id) {
    let dd = datadir(name);
    let db = UserDbs::new(dd.users_dir()).get(Id::new()).await.unwrap();
    let (s, _) = create_solution(&db, &format!("g-{name}"), "Решение")
        .await
        .unwrap();
    (db, s.id)
}

fn agent() -> Attribution {
    Attribution {
        author_kind: AuthorKind::Agent,
        token_id: Some(Id::new()),
        session_id: Some("s-rep".into()),
        machine: Some("box".into()),
        message: None,
        user_id: Some(Id::new()),
        area: None,
        solution: None,
    }
}

fn human() -> Attribution {
    Attribution {
        author_kind: AuthorKind::Human,
        token_id: None,
        session_id: None,
        machine: None,
        message: None,
        user_id: Some(Id::new()),
        area: None,
        solution: None,
    }
}

/// sha256 файла номер `n` (64 hex; первые 16 символов различаются и содержат буквы).
fn sha(n: u32) -> String {
    format!("{n:08x}{}", "ab".repeat(28))
}

fn file(n: u32, name: &str, mime: &str) -> NewReportFile {
    NewReportFile {
        sha256: sha(n),
        filename: name.into(),
        mime: mime.into(),
        size: 100 + u64::from(n),
    }
}

/// Тело со ссылками на файлы `files` (картинки — `![…]`, прочие — `[…]`) в порядке списка.
fn body_of(files: &[NewReportFile]) -> String {
    let mut s = String::from("## Итоги\n\nЛосс упал вдвое: $L = 0.5$.\n\n");
    for f in files {
        let link = attachment_link(&f.sha256, &f.filename);
        if f.mime.starts_with("image/") {
            s.push_str(&format!("![{}]({link})\n\n", f.filename));
        } else {
            s.push_str(&format!("[{}]({link})\n\n", f.filename));
        }
    }
    s
}

fn report(space: Id, title: &str, body: String) -> NewEntry {
    NewEntry {
        refs: None,
        space_id: space,
        kind: EntryKind::Report,
        title: title.into(),
        body,
        data: json!({}),
        area: Some("ml".into()),
        tags: vec!["обучение".into()],
        status: None,
        certainty: None,
        idempotency_key: None,
        triggers: Vec::new(),
        links: Vec::new(),
    }
}

fn changelog(space: Id, title: &str) -> NewEntry {
    NewEntry {
        refs: None,
        space_id: space,
        kind: EntryKind::Changelog,
        title: title.into(),
        body: "Подробности.".into(),
        data: json!({"kind": "fix"}),
        area: None,
        tags: Vec::new(),
        status: None,
        certainty: None,
        idempotency_key: None,
        triggers: Vec::new(),
        links: Vec::new(),
    }
}

fn three_files() -> Vec<NewReportFile> {
    vec![
        file(1, "loss.png", "image/png"),
        file(2, "acc.svg", "image/svg+xml"),
        file(3, "results.csv", "text/csv"),
    ]
}

/// Отчёт с тремя файлами.
async fn make_report(db: &Db, space: Id, title: &str) -> zam_store::Entry {
    let files = three_files();
    let (e, created) = create_report(db, report(space, title, body_of(&files)), files, &agent())
        .await
        .unwrap();
    assert!(created);
    e
}

fn filter(space: Id) -> EntryFilter {
    EntryFilter {
        space_id: space,
        kind: None,
        statuses: Vec::new(),
        certainty: Vec::new(),
        area: None,
        tag: None,
        data_kind: None,
        decided_by: None,
        since: None,
        until: None,
        cursor: None,
        limit: 50,
        with_reports: false,
        deleted_only: false,
    }
}

async fn count_sql(db: &Db, sql: &'static str, id: Option<Id>) -> i64 {
    db.read(move |c| {
        Ok(match id {
            Some(id) => c.query_row(sql, [id], |r| r.get(0))?,
            None => c.query_row(sql, [], |r| r.get(0))?,
        })
    })
    .await
    .unwrap()
}

async fn exec(db: &Db, sql: String) -> Result<usize, StoreError> {
    db.write(move |tx| Ok(tx.execute(&sql, [])?)).await
}

fn immutable_sql(r: Result<usize, StoreError>) -> bool {
    matches!(r, Err(StoreError::Sqlite(e)) if e.to_string().contains("report-immutable"))
}

fn check_failed(r: Result<usize, StoreError>) -> bool {
    matches!(r, Err(StoreError::Sqlite(e)) if e.to_string().contains("CHECK"))
}

async fn refcount(db: &Db, n: u32) -> Option<i64> {
    let s = sha(n);
    db.read(move |c| {
        use rusqlite::OptionalExtension;
        Ok(
            c.query_row("SELECT refcount FROM blobs WHERE sha256 = ?1", [s], |r| {
                r.get(0)
            })
            .optional()?,
        )
    })
    .await
    .unwrap()
}

async fn entry_jobs(db: &Db, id: Id) -> i64 {
    count_sql(
        db,
        "SELECT count(*) FROM index_jobs WHERE target_kind = 'entry' AND target_id = ?1",
        Some(id),
    )
    .await
}

async fn entry_chunks(db: &Db, id: Id) -> i64 {
    count_sql(
        db,
        "SELECT count(*) FROM chunks WHERE entry_id = ?1",
        Some(id),
    )
    .await
}

// ---------- миграция 0022 ----------

fn conn(path: &Path) -> Connection {
    let c = Connection::open(path).unwrap();
    c.pragma_update(None, "foreign_keys", true).unwrap();
    c
}

fn table_counts(c: &Connection) -> BTreeMap<String, i64> {
    let names: Vec<String> = {
        let mut st = c
            .prepare(
                "SELECT name FROM sqlite_master
                 WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
            )
            .unwrap();
        st.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    names
        .into_iter()
        .map(|n| {
            let k: i64 = c
                .query_row(&format!("SELECT count(*) FROM \"{n}\""), [], |r| r.get(0))
                .unwrap();
            (n, k)
        })
        .collect()
}

fn objects(c: &Connection, table: &str) -> Vec<String> {
    let mut st = c
        .prepare(
            "SELECT type || ' ' || name FROM sqlite_master
             WHERE tbl_name = ?1 AND type IN ('index', 'trigger') ORDER BY 1",
        )
        .unwrap();
    st.query_map([table], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// База на 0021 с записями всех видов, ревизиями, триггерами, замещением и корзиной
/// переходит на 0022 без потерь: строки, индексы `entries_*`, внешние ключи.
#[tokio::test]
async fn migration_0022_keeps_entries_and_indexes() {
    let dd = datadir("migration");
    let user = Id::new();
    let path = dd.user_db(&user);
    assert_eq!(USER_MIGRATIONS[20].version, 21);
    {
        let db = Db::open(&path, &USER_MIGRATIONS[..21]).unwrap();
        let (sol, kn, rules, hu) = (Id::new(), Id::new(), Id::new(), Id::new());
        let now = zam_store::time::fmt_ts(Timestamp::now());
        db.write(move |tx| {
            for (id, kind, guid) in [
                (sol, "solution", Some("g-mig")),
                (kn, "knowledge", None),
                (rules, "rules", None),
                (hu, "human", None),
            ] {
                tx.execute(
                    "INSERT INTO spaces (id, kind, solution_guid, name, created_at)
                     VALUES (?1, ?2, ?3, ?2, ?4)",
                    rusqlite::params![id, kind, guid, now],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
        let a = agent();
        let mut insights = Vec::new();
        for i in 0..30 {
            let e = match i % 3 {
                0 => changelog(sol, &format!("Изменение {i}")),
                1 => NewEntry {
                    kind: EntryKind::Task,
                    data: json!({}),
                    ..changelog(sol, &format!("Задача {i}"))
                },
                _ => NewEntry {
                    kind: EntryKind::Insight,
                    space_id: kn,
                    data: json!({"kind": "gotcha"}),
                    certainty: Some(Certainty::Hypothesis),
                    triggers: vec![NewTrigger {
                        kind: TriggerKind::Keyword,
                        pattern: format!("слово{i}"),
                        tools: None,
                    }],
                    ..changelog(kn, &format!("Инсайт {i}"))
                },
            };
            let (e, _) = create_entry(&db, e, &a).await.unwrap();
            update_entry(
                &db,
                e.id,
                None,
                EntryPatch {
                    append_body: Some("Ещё.".into()),
                    ..EntryPatch::default()
                },
                &a,
            )
            .await
            .unwrap();
            if e.kind == EntryKind::Insight {
                insights.push(e.id);
            }
        }
        let new = NewEntry {
            kind: EntryKind::Insight,
            space_id: kn,
            data: json!({"kind": "gotcha"}),
            refs: Some(Vec::new()),
            ..changelog(kn, "Замена")
        };
        supersede_entry(&db, insights[0], None, new, &a)
            .await
            .unwrap();
        delete_entry(&db, insights[1], None, &a).await.unwrap();
    }
    let (before, idx_before) = {
        let c = conn(&path);
        let v: u32 = c
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 21);
        (table_counts(&c), objects(&c, "entries"))
    };
    assert!(before["entries"] >= 31 && before["entry_revisions"] > before["entries"]);

    let dbs = UserDbs::new(dd.users_dir());
    let db = dbs.get(user).await.unwrap();
    dbs.wait_rechunk(user).await;
    let (after, idx_after, rev_objects, version, violations, sql) = db
        .read(|c| {
            let sql: String = c.query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'entries'",
                [],
                |r| r.get(0),
            )?;
            let mut st = c.prepare("PRAGMA foreign_key_check")?;
            let violations = st.query_map([], |_| Ok(()))?.count();
            let v: u32 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
            Ok((
                table_counts(c),
                objects(c, "entries"),
                objects(c, "entry_revisions"),
                v,
                violations,
                sql,
            ))
        })
        .await
        .unwrap();
    assert_eq!(version, USER_MIGRATIONS.len() as u32);
    assert!(version >= 22);
    assert_eq!(violations, 0, "foreign_key_check не пуст");
    assert!(sql.contains("'report'"), "{sql}");
    assert!(!after.contains_key("entries_new"));
    for (t, n) in before.iter().filter(|(t, _)| t.as_str() != "meta") {
        assert_eq!(after.get(t), Some(n), "таблица {t}");
    }
    assert_eq!(after.get("entry_attachments"), Some(&0));
    for i in ["entries_feed", "entries_title", "entries_pending"] {
        assert!(idx_after.contains(&format!("index {i}")), "{idx_after:?}");
    }
    // прежние индексы на месте, добавлен только триггер неизменности
    let mut expected = idx_before.clone();
    expected.push("trigger entries_report_immutable".into());
    expected.sort();
    assert_eq!(idx_after, expected);
    assert!(
        rev_objects.contains(&"trigger entry_revisions_report_once".to_string()),
        "{rev_objects:?}"
    );
    // ревизии и самоссылки замещения пережили пересборку
    let superseded = count_sql(
        &db,
        "SELECT count(*) FROM entries o JOIN entries n ON n.id = o.superseded_by_id
         WHERE n.supersedes_id = o.id",
        None,
    )
    .await;
    assert_eq!(superseded, 1);
}

/// CHECK 0022: у report нет статуса, `certainty` и замещения.
#[tokio::test]
async fn report_kind_check_status_certainty_null() {
    let (db, space) = setup("check").await;
    let insert = |cols: &str, vals: &str| {
        format!(
            "INSERT INTO entries (id, space_id, kind, title, title_norm, author_kind, created_at,
                                  updated_at{cols})
             VALUES ('{}', '{space}', 'report', 't', 't', 'agent', '2026-11-01T00:00:00Z',
                     '2026-11-01T00:00:00Z'{vals})",
            Id::new()
        )
    };
    assert!(check_failed(
        exec(&db, insert(", status", ", 'active'")).await
    ));
    assert!(check_failed(
        exec(&db, insert(", certainty", ", 'hypothesis'")).await
    ));
    assert!(check_failed(
        exec(&db, insert(", retract_reason", ", 'нет'")).await
    ));
    assert!(check_failed(
        exec(
            &db,
            insert(", superseded_by_id", &format!(", '{}'", Id::new()))
        )
        .await
    ));
    exec(&db, insert("", "")).await.unwrap();
    // у прочих видов правило статуса прежнее
    let task = format!(
        "INSERT INTO entries (id, space_id, kind, title, title_norm, author_kind, created_at,
                              updated_at)
         VALUES ('{}', '{space}', 'task', 't', 't', 'agent', '2026-11-01T00:00:00Z',
                 '2026-11-01T00:00:00Z')",
        Id::new()
    );
    assert!(check_failed(exec(&db, task).await));
}

#[tokio::test]
async fn create_report_atomic_with_attachments() {
    let (db, space) = setup("atomic").await;
    let e = make_report(&db, space, "Обучение новой архитектуры").await;
    assert_eq!(e.kind, EntryKind::Report);
    assert_eq!((e.rev, e.status, e.certainty), (1, None, None));
    assert_eq!(e.author.kind, AuthorKind::Agent);
    assert_eq!(entry_revisions(&db, e.id).await.unwrap().len(), 1);
    let att = entry_attachments(&db, e.id).await.unwrap();
    let names: Vec<&str> = att.iter().map(|a| a.filename.as_str()).collect();
    assert_eq!(names.len(), 3);
    for n in ["loss.png", "acc.svg", "results.csv"] {
        assert!(names.contains(&n), "{names:?}");
    }
    let csv = att.iter().find(|a| a.filename == "results.csv").unwrap();
    assert_eq!((csv.mime.as_str(), csv.size), ("text/csv", 103));
    for n in 1..=3 {
        assert_eq!(refcount(&db, n).await, Some(1));
    }

    // сбой посреди транзакции (после строки записи и первого файла) — ничего не записано
    let entries_before = count_sql(&db, "SELECT count(*) FROM entries", None).await;
    let chunks_before = count_sql(&db, "SELECT count(*) FROM chunks", None).await;
    exec(
        &db,
        "CREATE TRIGGER test_boom BEFORE INSERT ON entry_attachments
         WHEN NEW.filename = 'boom.png' BEGIN SELECT RAISE(ABORT, 'boom'); END"
            .into(),
    )
    .await
    .unwrap();
    let files = vec![
        file(10, "first.png", "image/png"),
        file(1, "loss.png", "image/png"),
        file(11, "boom.png", "image/png"),
    ];
    let r = create_report(&db, report(space, "Сбой", body_of(&files)), files, &agent()).await;
    assert!(
        matches!(&r, Err(StoreError::Sqlite(e)) if e.to_string().contains("boom")),
        "{r:?}"
    );
    exec(&db, "DROP TRIGGER test_boom".into()).await.unwrap();
    assert_eq!(
        count_sql(&db, "SELECT count(*) FROM entries", None).await,
        entries_before
    );
    assert_eq!(
        count_sql(&db, "SELECT count(*) FROM chunks", None).await,
        chunks_before
    );
    assert_eq!(
        count_sql(&db, "SELECT count(*) FROM entry_attachments", None).await,
        3
    );
    assert_eq!(refcount(&db, 10).await, None);
    assert_eq!(refcount(&db, 1).await, Some(1));
}

#[tokio::test]
async fn create_report_same_blob_twice_refcount() {
    let (db, space) = setup("refcount").await;
    let one = vec![file(7, "fig.png", "image/png")];
    let other = vec![file(7, "другое имя.png", "image/png")];
    let (a, _) = create_report(&db, report(space, "Первый", body_of(&one)), one, &agent())
        .await
        .unwrap();
    let (b, _) = create_report(
        &db,
        report(space, "Второй", body_of(&other)),
        other,
        &agent(),
    )
    .await
    .unwrap();
    assert_ne!(a.id, b.id);
    assert_eq!(
        count_sql(&db, "SELECT count(*) FROM blobs", None).await,
        1,
        "одна картинка — один блоб"
    );
    assert_eq!(refcount(&db, 7).await, Some(2));
    // повтор одной пары в списке файлов — одна строка и одна единица счётчика
    let dup = vec![file(8, "x.png", "image/png"), file(8, "x.png", "image/png")];
    let (c, _) = create_report(
        &db,
        report(space, "Третий", body_of(&dup[..1])),
        dup,
        &agent(),
    )
    .await
    .unwrap();
    assert_eq!(entry_attachments(&db, c.id).await.unwrap().len(), 1);
    assert_eq!(refcount(&db, 8).await, Some(1));
}

#[tokio::test]
async fn report_index_state_excluded_fts_only() {
    let (db, space) = setup("fts").await;
    let files = three_files();
    let mut new = report(space, "Квазиуникальный отчёт", body_of(&files));
    new.body
        .push_str("Сравнение с бейзлайном: трансформерлинейный выигрывает.\n");
    let (e, _) = create_report(&db, new, files, &agent()).await.unwrap();
    assert_eq!(e.index_state, IndexState::Excluded);
    assert!(entry_chunks(&db, e.id).await > 0);
    assert_eq!(entry_jobs(&db, e.id).await, 0);
    let fts = |kinds: Vec<String>| {
        let db = db.clone();
        async move {
            search_fts(
                &db,
                FtsQuery {
                    alias_groups: Vec::new(),
                    text: "трансформерлинейный".into(),
                    space_ids: vec![space],
                    path_prefix: None,
                    top_k: 10,
                    targets: TargetFilter {
                        kinds,
                        ..TargetFilter::default()
                    },
                },
            )
            .await
            .unwrap()
        }
    };
    // FTS находит отчёт по явному виду (поиск WebUI); без вида — нет (умолчание агента)
    let hits = fts(vec!["report".into()]).await;
    assert!(
        hits.iter()
            .any(|h| h.target.id == e.id && h.target.kind == TargetKind::Entry),
        "{hits:?}"
    );
    assert!(fts(Vec::new()).await.is_empty());
    assert!(
        fts(vec!["changelog".into(), "note".into()])
            .await
            .is_empty()
    );
    assert!(
        zam_store::entry_chunks_for_index(&db, e.id)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn report_never_enqueued_on_rechunk_or_reindex() {
    let (db, space) = setup("reindex").await;
    let e = make_report(&db, space, "Отчёт").await;
    let (c, _) = create_entry(&db, changelog(space, "Изменение"), &agent())
        .await
        .unwrap();
    for pass in 0..2 {
        if pass == 1 {
            // даже с `pending` (прямой правкой) отчёт не индексируется векторами
            exec(
                &db,
                format!(
                    "UPDATE entries SET index_state = 'pending' WHERE id = '{}'",
                    e.id
                ),
            )
            .await
            .unwrap();
        }
        enqueue_reindex(&db, None).await.unwrap();
        assert_eq!(entry_jobs(&db, e.id).await, 0, "проход {pass}");
        assert!(entry_jobs(&db, c.id).await > 0);
        let page = reindex_entries_page(&db, None, None, 100).await.unwrap();
        assert!(page.iter().all(|p| p.entry_id != e.id));
        assert!(page.iter().any(|p| p.entry_id == c.id));
        assert_eq!(enqueue_entry_upserts(&db, vec![e.id]).await.unwrap(), 0);
        assert_eq!(entry_jobs(&db, e.id).await, 0);
        assert!(entry_chunks(&db, e.id).await > 0, "FTS-чанки остаются");
    }
}

#[tokio::test]
async fn update_supersede_retract_report_immutable() {
    let (db, space) = setup("immutable").await;
    let e = make_report(&db, space, "Неизменяемый").await;
    let a = agent();
    let patches: Vec<(&str, EntryPatch)> = vec![
        ("пустая", EntryPatch::default()),
        (
            "title",
            EntryPatch {
                title: Some("Другой".into()),
                ..EntryPatch::default()
            },
        ),
        (
            "body",
            EntryPatch {
                body: Some("x".into()),
                ..EntryPatch::default()
            },
        ),
        (
            "append_body",
            EntryPatch {
                append_body: Some("x".into()),
                ..EntryPatch::default()
            },
        ),
        (
            "data",
            EntryPatch {
                data: Some(json!({"refs": {"commits": ["abc"]}})),
                ..EntryPatch::default()
            },
        ),
        (
            "tags",
            EntryPatch {
                tags: Some(vec!["x".into()]),
                ..EntryPatch::default()
            },
        ),
        (
            "area",
            EntryPatch {
                area: Some(None),
                ..EntryPatch::default()
            },
        ),
        (
            "status",
            EntryPatch {
                status: Some(EntryStatus::Active),
                ..EntryPatch::default()
            },
        ),
        (
            "certainty",
            EntryPatch {
                certainty: Some(Some(Certainty::Hypothesis)),
                ..EntryPatch::default()
            },
        ),
        (
            "reason",
            EntryPatch {
                reason: Some("потому что".into()),
                ..EntryPatch::default()
            },
        ),
        (
            "triggers",
            EntryPatch {
                triggers: Some(Vec::new()),
                ..EntryPatch::default()
            },
        ),
        (
            "refs",
            EntryPatch {
                refs: Some(vec![FileRef::path_only("src/lib.rs".into())]),
                ..EntryPatch::default()
            },
        ),
    ];
    for (name, p) in patches {
        let r = update_entry(&db, e.id, Some(e.rev), p, &a).await;
        assert!(
            matches!(r, Err(StoreError::ReportImmutable)),
            "{name}: {r:?}"
        );
    }
    let r = supersede_entry(&db, e.id, None, report(space, "Новый", "т".into()), &a).await;
    assert!(matches!(r, Err(StoreError::ReportImmutable)), "{r:?}");
    let r = retract_entry(&db, e.id, None, "ошибка", &a).await;
    assert!(matches!(r, Err(StoreError::ReportImmutable)), "{r:?}");
    let t = vec![NewTrigger {
        kind: TriggerKind::Keyword,
        pattern: "слово".into(),
        tools: None,
    }];
    let r = set_triggers(&db, e.id, t, AuthorKind::Human).await;
    assert!(matches!(r, Err(StoreError::ReportImmutable)), "{r:?}");
    let r = claim_task(&db, e.id, "s", None, 10, Timestamp::now()).await;
    assert!(
        matches!(r, Err(TaskError::Store(StoreError::ReportImmutable))),
        "{r:?}"
    );
    let r = release_task(&db, e.id, "s", true).await;
    assert!(
        matches!(r, Err(TaskError::Store(StoreError::ReportImmutable))),
        "{r:?}"
    );
    let ans = NewAnswer {
        text: Some("да".into()),
        option: None,
        by: AnswerBy::Human,
        channel: Channel::Webui,
    };
    let r = answer_question(&db, e.id, ans, &human()).await;
    assert!(
        matches!(r, Err(QuestionError::Store(StoreError::ReportImmutable))),
        "{r:?}"
    );
    assert!(
        StoreError::ReportImmutable
            .to_string()
            .contains("неизменяем")
    );
    let now = get_entry(&db, e.id).await.unwrap();
    assert_eq!((now.rev, &now.body, &now.tags), (1, &e.body, &e.tags));
    assert_eq!(entry_revisions(&db, e.id).await.unwrap().len(), 1);
}

#[tokio::test]
async fn sql_update_report_content_aborts() {
    let (db, space) = setup("sql").await;
    let e = make_report(&db, space, "Отчёт").await;
    let id = e.id;
    for set in [
        "body = 'x'",
        "title = 'x'",
        "title_norm = 'x'",
        "data = '{\"a\":1}'",
        "tags = '[]'",
        "area = NULL",
        "current_rev = 2",
        "kind = 'changelog'",
        "space_id = 'x'",
        "author_kind = 'human'",
        "created_at = '2020-01-01T00:00:00Z'",
    ] {
        let r = exec(&db, format!("UPDATE entries SET {set} WHERE id = '{id}'")).await;
        assert!(immutable_sql(r), "{set}");
    }
    // корзина, время и состояние индекса меняются
    for set in [
        "deleted_at = '2026-11-02T00:00:00Z'",
        "deleted_at = NULL",
        "updated_at = '2026-11-02T00:00:00Z'",
        "index_state = 'pending'",
        "index_state = 'excluded'",
    ] {
        let n = exec(&db, format!("UPDATE entries SET {set} WHERE id = '{id}'"))
            .await
            .unwrap();
        assert_eq!(n, 1, "{set}");
    }
    // правка прочих записей не задета
    let (c, _) = create_entry(&db, changelog(space, "Изменение"), &agent())
        .await
        .unwrap();
    exec(
        &db,
        format!("UPDATE entries SET body = 'новое' WHERE id = '{}'", c.id),
    )
    .await
    .unwrap();
    assert_eq!(get_entry(&db, id).await.unwrap().body, e.body);
}

#[tokio::test]
async fn report_revision_only_first() {
    let (db, space) = setup("revision").await;
    let e = make_report(&db, space, "Отчёт").await;
    let rev = |id: Id, n: i64| {
        format!(
            "INSERT INTO entry_revisions (entry_id, rev, title, body, data, tags, ts, author_kind)
             VALUES ('{id}', {n}, 't', 'b', '{{}}', '[]', '2026-11-02T00:00:00Z', 'agent')"
        )
    };
    assert!(immutable_sql(exec(&db, rev(e.id, 2)).await));
    // у других видов ревизии дописываются
    let (c, _) = create_entry(&db, changelog(space, "Изменение"), &agent())
        .await
        .unwrap();
    exec(&db, rev(c.id, 2)).await.unwrap();
}

#[tokio::test]
async fn entry_attachments_append_only() {
    let (db, space) = setup("append").await;
    let e = make_report(&db, space, "Отчёт").await;
    let r = exec(
        &db,
        format!(
            "UPDATE entry_attachments SET filename = 'x.png' WHERE entry_id = '{}'",
            e.id
        ),
    )
    .await;
    assert!(immutable_sql(r));
    let r = exec(
        &db,
        format!("DELETE FROM entry_attachments WHERE entry_id = '{}'", e.id),
    )
    .await;
    assert!(immutable_sql(r));
    assert_eq!(entry_attachments(&db, e.id).await.unwrap().len(), 3);
}

#[tokio::test]
async fn create_entry_rejects_report_kind() {
    let (db, space) = setup("reject").await;
    let r = create_entry(&db, report(space, "Отчёт", "т".into()), &agent()).await;
    assert!(
        matches!(&r, Err(StoreError::BadInput(m)) if m.contains("create_report")),
        "{r:?}"
    );
    // create_report принимает только report, только в пространстве решения, без статуса
    let r = create_report(&db, changelog(space, "x"), Vec::new(), &agent()).await;
    assert!(matches!(r, Err(StoreError::BadInput(_))), "{r:?}");
    let kn = list_spaces(&db)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.kind == SpaceKind::Knowledge)
        .unwrap()
        .id;
    let r = create_report(&db, report(kn, "x", "т".into()), Vec::new(), &agent()).await;
    assert!(matches!(r, Err(StoreError::BadInput(_))), "{r:?}");
    let mut st = report(space, "x", "т".into());
    st.status = Some(EntryStatus::Active);
    let r = create_report(&db, st, Vec::new(), &agent()).await;
    assert!(matches!(r, Err(StoreError::BadInput(_))), "{r:?}");
    let mut ce = report(space, "x", "т".into());
    ce.certainty = Some(Certainty::Hypothesis);
    let r = create_report(&db, ce, Vec::new(), &agent()).await;
    assert!(matches!(r, Err(StoreError::BadInput(_))), "{r:?}");
    let r = create_report(
        &db,
        report(space, "x", "т".into()),
        vec![file(1, "a/b.png", "image/png")],
        &agent(),
    )
    .await;
    assert!(matches!(r, Err(StoreError::BadInput(_))), "{r:?}");
    let mut bad_sha = file(1, "a.png", "image/png");
    bad_sha.sha256 = "ABC".into();
    let r = create_report(&db, report(space, "x", "т".into()), vec![bad_sha], &agent()).await;
    assert!(matches!(r, Err(StoreError::BadInput(_))), "{r:?}");
    assert_eq!(
        count_sql(&db, "SELECT count(*) FROM entries", None).await,
        0
    );
}

#[tokio::test]
async fn report_idempotency_key_fingerprint_includes_files() {
    let (db, space) = setup("idem").await;
    let files = three_files();
    let mut new = report(space, "Отчёт", body_of(&files));
    new.idempotency_key = Some("k1".into());
    let a = agent();
    let (e, created) = create_report(&db, new.clone(), files.clone(), &a)
        .await
        .unwrap();
    assert!(created);
    // тот же запрос, файлы в другом порядке — та же запись
    let mut rev_files = files.clone();
    rev_files.reverse();
    let (again, created) = create_report(&db, new.clone(), rev_files, &a)
        .await
        .unwrap();
    assert_eq!((again.id, created), (e.id, false));
    // другой набор файлов — расхождение
    let r = create_report(&db, new.clone(), files[..2].to_vec(), &a).await;
    assert!(
        matches!(r, Err(StoreError::IdempotencyMismatch(_))),
        "{r:?}"
    );
    let mut renamed = files.clone();
    renamed[0].filename = "other.png".into();
    let r = create_report(&db, new, renamed, &a).await;
    assert!(
        matches!(r, Err(StoreError::IdempotencyMismatch(_))),
        "{r:?}"
    );
    for n in 1..=3 {
        assert_eq!(refcount(&db, n).await, Some(1));
    }
}

#[tokio::test]
async fn report_links_refs_rejected_no_file_refs_no_edges() {
    let (db, space) = setup("links").await;
    let a = agent();
    let (c, _) = create_entry(&db, changelog(space, "Изменение"), &a)
        .await
        .unwrap();
    let target = TargetRef {
        kind: TargetKind::Entry,
        id: c.id,
    };
    let mut l = report(space, "x", "т".into());
    l.links = vec![(EdgeType::RelatesTo, target)];
    let r = create_report(&db, l, Vec::new(), &a).await;
    assert!(matches!(r, Err(StoreError::BadInput(_))), "{r:?}");
    let mut rf = report(space, "x", "т".into());
    rf.refs = Some(Vec::new());
    let r = create_report(&db, rf, Vec::new(), &a).await;
    assert!(matches!(r, Err(StoreError::BadInput(_))), "{r:?}");
    let mut t = report(space, "x", "т".into());
    t.triggers = vec![NewTrigger {
        kind: TriggerKind::Keyword,
        pattern: "слово".into(),
        tools: None,
    }];
    let r = create_report(&db, t, Vec::new(), &a).await;
    assert!(matches!(r, Err(StoreError::BadInput(_))), "{r:?}");

    // `data.refs` — только данные: строк `file_refs` и рёбер нет
    let mut ok = report(space, "С ссылками", "т".into());
    ok.data = json!({"refs": {"commits": ["abc1234"], "files": ["src/lib.rs"],
                              "notes": [c.id.to_string()], "tasks": []}});
    let (e, _) = create_report(&db, ok, Vec::new(), &a).await.unwrap();
    assert_eq!(e.data["refs"]["files"], json!(["src/lib.rs"]));
    assert_eq!(
        count_sql(
            &db,
            "SELECT count(*) FROM file_refs WHERE owner_id = ?1",
            Some(e.id)
        )
        .await,
        0
    );
    let rep = TargetRef {
        kind: TargetKind::Entry,
        id: e.id,
    };
    for (src, dst) in [(rep, target), (target, rep)] {
        let r = db
            .add_edge(
                NewEdge {
                    src,
                    dst,
                    kind: EdgeType::RelatesTo,
                },
                &a,
            )
            .await;
        assert!(matches!(r, Err(StoreError::BadInput(_))), "{r:?}");
    }
    assert_eq!(
        count_sql(
            &db,
            "SELECT count(*) FROM edges WHERE src_id = ?1 OR dst_id = ?1",
            Some(e.id)
        )
        .await,
        0
    );
}

#[tokio::test]
async fn list_entries_hides_reports_unless_asked() {
    let (db, space) = setup("list").await;
    let (c, _) = create_entry(&db, changelog(space, "Изменение"), &agent())
        .await
        .unwrap();
    let r = make_report(&db, space, "Отчёт").await;
    let ids = |v: Vec<zam_store::Entry>| v.into_iter().map(|e| e.id).collect::<Vec<_>>();
    let (items, _) = list_entries(&db, filter(space)).await.unwrap();
    assert_eq!(ids(items), vec![c.id]);
    let (items, _) = list_entries(
        &db,
        EntryFilter {
            with_reports: true,
            ..filter(space)
        },
    )
    .await
    .unwrap();
    assert_eq!(ids(items), vec![r.id, c.id]);
    let (items, _) = list_entries(
        &db,
        EntryFilter {
            kind: Some(EntryKind::Report),
            ..filter(space)
        },
    )
    .await
    .unwrap();
    assert_eq!(ids(items), vec![r.id]);
}

#[tokio::test]
async fn list_entries_deleted_only() {
    let (db, space) = setup("trash").await;
    let a = agent();
    let mut all = Vec::new();
    for i in 0..4 {
        let (e, _) = create_entry(&db, changelog(space, &format!("Изменение {i}")), &a)
            .await
            .unwrap();
        all.push(e.id);
    }
    let rep = make_report(&db, space, "Отчёт").await;
    // удаление в обратном порядке создания: корзина — по времени удаления
    for id in [all[0], all[2]] {
        delete_entry(&db, id, None, &a).await.unwrap();
    }
    delete_entry(&db, rep.id, None, &a).await.unwrap();
    let trash = EntryFilter {
        deleted_only: true,
        ..filter(space)
    };
    let (items, next) = list_entries(&db, trash.clone()).await.unwrap();
    assert!(next.is_none());
    let got: Vec<Id> = items.iter().map(|e| e.id).collect();
    assert_eq!(got, vec![all[2], all[0]]);
    assert!(items.iter().all(|e| e.deleted_at.is_some()));
    // корзина отчётов
    let (items, _) = list_entries(
        &db,
        EntryFilter {
            kind: Some(EntryKind::Report),
            ..trash.clone()
        },
    )
    .await
    .unwrap();
    assert_eq!(items.iter().map(|e| e.id).collect::<Vec<_>>(), vec![rep.id]);
    // курсор по `deleted_at DESC, id`
    let mut f = EntryFilter {
        limit: 1,
        with_reports: true,
        ..trash
    };
    let mut pages = Vec::new();
    loop {
        let (items, next) = list_entries(&db, f.clone()).await.unwrap();
        pages.extend(items.into_iter().map(|e| e.id));
        match next {
            Some(n) => f.cursor = Some(n),
            None => break,
        }
    }
    assert_eq!(pages, vec![rep.id, all[2], all[0]]);
    // живая лента корзины не видит
    let (items, _) = list_entries(&db, filter(space)).await.unwrap();
    assert_eq!(
        items.iter().map(|e| e.id).collect::<Vec<_>>(),
        vec![all[3], all[1]]
    );
}

#[tokio::test]
async fn delete_restore_report_keeps_attachments() {
    let (db, space) = setup("delete").await;
    let e = make_report(&db, space, "Отчёт").await;
    let chunks = entry_chunks(&db, e.id).await;
    assert!(chunks > 0);
    delete_entry(&db, e.id, Some(1), &human()).await.unwrap();
    let d = get_entry(&db, e.id).await.unwrap();
    assert!(d.deleted_at.is_some());
    assert_eq!(entry_chunks(&db, e.id).await, 0);
    assert_eq!(entry_attachments(&db, e.id).await.unwrap().len(), 3);
    for n in 1..=3 {
        assert_eq!(refcount(&db, n).await, Some(1));
    }
    let r = restore_entry(&db, e.id, None, &human()).await.unwrap();
    assert!(r.deleted_at.is_none());
    assert_eq!((r.rev, r.index_state), (1, IndexState::Excluded));
    assert_eq!(entry_chunks(&db, e.id).await, chunks);
    assert_eq!(entry_attachments(&db, e.id).await.unwrap().len(), 3);
    assert_eq!(entry_jobs(&db, e.id).await, 0);
}

#[test]
fn ref_letter_p_roundtrip() {
    assert_eq!(EntryKind::Report.as_str(), "report");
    assert_eq!(EntryKind::parse("report"), Some(EntryKind::Report));
    assert_eq!(EntryKind::Report.ref_letter(), "p");
    assert_eq!(EntryKind::from_ref_letter("p"), Some(EntryKind::Report));
    assert_eq!(
        zam_store::ENTRY_REF_LETTERS,
        ["i", "c", "r", "t", "k", "q", "p", "e"]
    );
    assert!(zam_store::is_entry_ref_letter("p"));
    assert_eq!(EntryKind::ALL.len(), 7);
    let mut letters: Vec<&str> = EntryKind::ALL.iter().map(|k| k.ref_letter()).collect();
    for l in &letters {
        assert!(zam_store::ENTRY_REF_LETTERS.contains(l), "{l}");
    }
    letters.sort();
    letters.dedup();
    assert_eq!(letters.len(), EntryKind::ALL.len(), "буквы не повторяются");
    for k in EntryKind::ALL {
        assert_eq!(k.agent_visible(), k != EntryKind::Report, "{k:?}");
    }
    assert!(zam_store::allowed_statuses(EntryKind::Report).is_empty());
    assert_eq!(zam_store::initial_status(EntryKind::Report), None);
    assert!(!zam_store::has_certainty(EntryKind::Report));
    assert_eq!(
        serde_json::to_value(EntryKind::Report).unwrap(),
        json!("report")
    );
}

#[tokio::test]
async fn entry_attachment_lookup_by_sha16_name() {
    let (db, space) = setup("lookup").await;
    let e = make_report(&db, space, "Отчёт").await;
    let other = make_report(&db, space, "Другой").await;
    let s16 = sha(1)[..16].to_string();
    let a = entry_attachment(&db, e.id, &s16, "loss.png")
        .await
        .unwrap()
        .unwrap();
    assert_eq!((a.entry_id, a.sha256.as_str()), (e.id, sha(1).as_str()));
    assert_eq!(a.mime, "image/png");
    // регистр хэша не важен
    let upper = s16.to_ascii_uppercase();
    assert!(
        entry_attachment(&db, e.id, &upper, "loss.png")
            .await
            .unwrap()
            .is_some()
    );
    let unknown = sha(0xabcdef)[..16].to_string();
    assert!(
        entry_attachment(&db, e.id, &unknown, "loss.png")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        entry_attachment(&db, e.id, &s16, "LOSS.png")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        entry_attachment(&db, e.id, &s16[..15], "loss.png")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        entry_attachment(&db, e.id, "zzzzzzzzzzzzzzzz", "loss.png")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        entry_attachment(&db, Id::new(), &s16, "loss.png")
            .await
            .unwrap()
            .is_none()
    );
    // файл чужого отчёта находится только по своему отчёту
    let o = entry_attachment(&db, other.id, &s16, "loss.png")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(o.entry_id, other.id);
    // сериализация: sha256 и время
    let v = serde_json::to_value(&a).unwrap();
    assert_eq!(v["sha256"], json!(sha(1)));
    assert!(v["created_at"].as_str().unwrap().ends_with('Z'));
}

#[tokio::test]
async fn first_images_picks_first_image_by_body_order() {
    let (db, space) = setup("first").await;
    let a = agent();
    // порядок файлов — a, b, csv; в теле — csv, картинка без файла, b, a
    let files = vec![
        file(21, "a.png", "image/png"),
        file(22, "b.webp", "image/webp"),
        file(23, "data.csv", "text/csv"),
    ];
    let body = format!(
        "[данные]({})\n\n![нет](_attachments/{}/missing.png)\n\n![b]({})\n\n![a]({})\n",
        attachment_link(&sha(23), "data.csv"),
        &sha(99)[..16],
        attachment_link(&sha(22), "b.webp"),
        attachment_link(&sha(21), "a.png"),
    );
    let (r1, _) = create_report(&db, report(space, "Первый", body), files, &a)
        .await
        .unwrap();
    // картинка-ссылка на CSV (не image MIME) не миниатюра
    let only_csv = vec![file(24, "t.csv", "text/csv")];
    let body2 = format!("![таблица]({})\n", attachment_link(&sha(24), "t.csv"));
    let (r2, _) = create_report(&db, report(space, "Без картинок", body2), only_csv, &a)
        .await
        .unwrap();
    // имя с пробелом и скобками — percent-кодирование ссылки
    let spaced = vec![file(25, "loss (v2).png", "image/png")];
    let body3 = body_of(&spaced);
    let (r3, _) = create_report(&db, report(space, "Пробелы", body3), spaced, &a)
        .await
        .unwrap();
    let m = first_images(&db, &[r1.id, r2.id, r3.id, Id::new()])
        .await
        .unwrap();
    assert_eq!(m.len(), 2, "{m:?}");
    assert_eq!(m[&r1.id].filename, "b.webp");
    assert_eq!(m[&r3.id].filename, "loss (v2).png");
    assert!(!m.contains_key(&r2.id));
    assert!(first_images(&db, &[]).await.unwrap().is_empty());
}
