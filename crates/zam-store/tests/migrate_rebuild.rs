//! Миграции с пересборкой таблиц (`Migration::rebuild`, спецификация M4.5 §1.1) и миграция
//! 0015 базы пользователя (виды `term`/`task`/`question`, связь `contradicts`, §1.2–1.3) на
//! базе M4 (0014) с данными — Review Focus 1 плана M4.5.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use rusqlite::Connection;
use serde_json::json;
use zam_core::{Id, TargetKind, TargetRef};
use zam_store::{
    Attribution, AuthorKind, Certainty, DataDir, Db, EdgeType, EntryKind, EntryPatch, FtsQuery,
    Migration, NewEntry, NewTrigger, StoreError, TargetFilter, TriggerKind, USER_MIGRATIONS,
    UserDbs, create_entry, delete_entry, search_fts, supersede_entry, update_entry,
};

fn tmp_dir(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("migrate-rebuild")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Соединение как у [`Db`]: внешние ключи включены.
fn conn(path: &Path) -> Connection {
    let c = Connection::open(path).unwrap();
    c.pragma_update(None, "foreign_keys", true).unwrap();
    c
}

fn fk_on(c: &Connection) -> bool {
    c.query_row("PRAGMA foreign_keys", [], |r| r.get(0))
        .unwrap()
}

fn user_version(c: &Connection) -> u32 {
    c.query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap()
}

fn count(c: &Connection, table: &str) -> i64 {
    c.query_row(&format!("SELECT count(*) FROM \"{table}\""), [], |r| {
        r.get(0)
    })
    .unwrap()
}

fn fk_violations(c: &Connection) -> usize {
    let mut st = c.prepare("PRAGMA foreign_key_check").unwrap();
    st.query_map([], |_| Ok(())).unwrap().count()
}

/// Родитель с CHECK и дочерняя таблица с FK на него (версия 1).
const PARENT_CHILD: &str = "
    CREATE TABLE parent (id INTEGER PRIMARY KEY, v TEXT NOT NULL CHECK (v IN ('a', 'b')));
    CREATE TABLE child (id INTEGER PRIMARY KEY, pid INTEGER NOT NULL REFERENCES parent (id));
    CREATE INDEX child_pid ON child (pid);
    INSERT INTO parent VALUES (1, 'a'), (2, 'b');
    INSERT INTO child VALUES (10, 1), (11, 1), (12, 2);";

/// Пересборка родителя с расширенным CHECK (версия 2).
const REBUILD_PARENT: &str = "
    CREATE TABLE parent_new (id INTEGER PRIMARY KEY,
                             v TEXT NOT NULL CHECK (v IN ('a', 'b', 'c')));
    INSERT INTO parent_new (id, v) SELECT id, v FROM parent;
    DROP TABLE parent;
    ALTER TABLE parent_new RENAME TO parent;";

static KEEP: &[Migration] = &[
    Migration {
        version: 1,
        sql: PARENT_CHILD,
        rebuild: false,
    },
    Migration {
        version: 2,
        sql: REBUILD_PARENT,
        rebuild: true,
    },
];

/// Миграция оставляет висячую ссылку ребёнка.
static DANGLING: &[Migration] = &[
    Migration {
        version: 1,
        sql: PARENT_CHILD,
        rebuild: false,
    },
    Migration {
        version: 2,
        sql: "CREATE TABLE parent_new (id INTEGER PRIMARY KEY, v TEXT NOT NULL);
              INSERT INTO parent_new (id, v) SELECT id, v FROM parent WHERE id <> 1;
              DROP TABLE parent;
              ALTER TABLE parent_new RENAME TO parent;",
        rebuild: true,
    },
];

/// Пересборка родителя сохраняет строки детей, FK ребёнка указывает на новую таблицу, новый
/// CHECK действует.
#[test]
fn rebuild_keeps_child_rows() {
    let path = tmp_dir("keep").join("db.sqlite");
    let mut c = conn(&path);
    assert_eq!(
        zam_store::migrate::apply(&mut c, &path, &KEEP[..1]).unwrap(),
        1
    );
    // без признака `rebuild` DROP родителя упирается во внешний ключ ребёнка
    {
        let tx = c.transaction().unwrap();
        assert!(tx.execute_batch(REBUILD_PARENT).is_err());
    }
    assert_eq!(zam_store::migrate::apply(&mut c, &path, KEEP).unwrap(), 2);
    assert_eq!(user_version(&c), 2);
    assert_eq!(count(&c, "child"), 3);
    assert_eq!(count(&c, "parent"), 2);
    assert_eq!(fk_violations(&c), 0);
    c.execute("INSERT INTO parent VALUES (3, 'c')", []).unwrap();
    assert!(c.execute("INSERT INTO child VALUES (13, 99)", []).is_err());
    assert!(path.with_file_name("db.sqlite.bak-v1").exists());
}

/// Висячая ссылка после пересборки — `InvalidMigrations`, транзакция откатывается.
#[test]
fn rebuild_fk_violation_rolls_back() {
    let path = tmp_dir("dangling").join("db.sqlite");
    let mut c = conn(&path);
    zam_store::migrate::apply(&mut c, &path, &DANGLING[..1]).unwrap();
    let r = zam_store::migrate::apply(&mut c, &path, DANGLING);
    match r {
        Err(StoreError::InvalidMigrations(m)) => assert!(m.contains("child"), "{m}"),
        other => panic!("ожидалась InvalidMigrations: {other:?}"),
    }
    assert_eq!(user_version(&c), 1);
    assert_eq!(count(&c, "parent"), 2);
    assert_eq!(count(&c, "child"), 3);
    assert!(fk_on(&c));
    assert!(path.with_file_name("db.sqlite.bak-v1").exists());
}

/// После миграции с пересборкой проверки внешних ключей снова включены.
#[test]
fn rebuild_restores_foreign_keys_on() {
    let path = tmp_dir("fk-on").join("db.sqlite");
    let mut c = conn(&path);
    zam_store::migrate::apply(&mut c, &path, KEEP).unwrap();
    assert!(fk_on(&c));
    assert!(c.execute("INSERT INTO child VALUES (20, 42)", []).is_err());
}

// ---------- 0015 на базе M4 ----------

fn agent() -> Attribution {
    Attribution {
        author_kind: AuthorKind::Agent,
        token_id: Some(Id::new()),
        session_id: Some("s-m4".into()),
        machine: Some("box".into()),
        message: None,
        user_id: Some(Id::new()),
        area: None,
    }
}

fn new_entry(space: Id, kind: EntryKind, title: String, data: serde_json::Value) -> NewEntry {
    NewEntry {
        space_id: space,
        kind,
        title,
        body: "Тело записи для чанков и FTS.".into(),
        data,
        area: Some("store".into()),
        tags: vec!["m4".into()],
        status: None,
        certainty: (kind == EntryKind::Insight).then_some(Certainty::Hypothesis),
        idempotency_key: None,
        triggers: Vec::new(),
        links: Vec::new(),
    }
}

fn entry_ref(id: Id) -> TargetRef {
    TargetRef {
        kind: TargetKind::Entry,
        id,
    }
}

/// Таблицы базы (кроме служебных SQLite) и число строк.
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
        .map(|n| (n.clone(), count(c, &n)))
        .collect()
}

/// Индексы и триггеры таблиц `t` (с автоиндексами).
fn schema_objects(c: &Connection, tables: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for t in tables {
        let mut st = c
            .prepare(
                "SELECT type || ' ' || name FROM sqlite_master
                 WHERE tbl_name = ?1 AND type IN ('index', 'trigger') ORDER BY 1",
            )
            .unwrap();
        let rows: Vec<String> = st
            .query_map([t], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        out.extend(rows.into_iter().map(|r| format!("{t}: {r}")));
    }
    out
}

/// Заполняет базу на 0014: 300 записей трёх видов с ревизиями, 50 триггеров, рёбра всех
/// девяти типов, чанки записей (с FTS) и `usage`. Возвращает id записи с уникальным словом в
/// заголовке.
async fn fill_m4(db: &Db) -> Id {
    let now = zam_store::time::fmt_ts(Timestamp::now());
    let (rules, human, knowledge, solution) = (Id::new(), Id::new(), Id::new(), Id::new());
    let ts = now.clone();
    db.write(move |tx| {
        for (id, kind, guid, name) in [
            (rules, "rules", None, "Rules"),
            (human, "human", None, "Human"),
            (knowledge, "knowledge", None, "Knowledge"),
            (solution, "solution", Some("g-m45-rebuild"), "sol"),
        ] {
            tx.execute(
                "INSERT INTO spaces (id, kind, solution_guid, name, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![id, kind, guid, name, ts],
            )?;
        }
        Ok(())
    })
    .await
    .unwrap();
    let a = agent();
    let mut insights: Vec<Id> = Vec::new();
    let mut marked = None;
    let explicit: Vec<EdgeType> = EdgeType::ALL
        .into_iter()
        .filter(|t| !matches!(t, EdgeType::Supersedes | EdgeType::Contradicts))
        .collect();
    assert_eq!(explicit.len(), 8);
    for i in 0..300usize {
        let (kind, space, data) = match i % 3 {
            0 => (EntryKind::Changelog, solution, json!({"kind": "fix"})),
            1 => (
                EntryKind::Release,
                solution,
                json!({"version": format!("1.{i}.0")}),
            ),
            _ => (EntryKind::Insight, knowledge, json!({"kind": "gotcha"})),
        };
        let title = if i == 77 {
            "Квазиуникальный заголовок пересборки".to_string()
        } else {
            format!("Запись {i} вида {}", kind.as_str())
        };
        let mut e = new_entry(space, kind, title, data);
        if kind == EntryKind::Insight {
            // 25 insight по 2 триггера — 50 триггеров
            if insights.len() < 25 {
                e.triggers = vec![
                    NewTrigger {
                        kind: TriggerKind::Keyword,
                        pattern: format!("слово{i}"),
                        tools: None,
                    },
                    NewTrigger {
                        kind: TriggerKind::FileGlob,
                        pattern: format!("src/**/m{i}.rs"),
                        tools: Some(vec!["Edit".into()]),
                    },
                ];
            }
            // связь с предыдущим insight: восемь явных типов по кругу
            if let Some(prev) = insights.last() {
                e.links = vec![(explicit[insights.len() % 8], entry_ref(*prev))];
            }
        }
        let (created, _) = create_entry(db, e, &a).await.unwrap();
        if i == 77 {
            marked = Some(created.id);
        }
        // ревизии: каждая пятая запись правится
        if i % 5 == 0 {
            update_entry(
                db,
                created.id,
                Some(created.rev),
                EntryPatch {
                    append_body: Some(format!("Дополнение {i}.")),
                    ..EntryPatch::default()
                },
                &a,
            )
            .await
            .unwrap();
        }
        if kind == EntryKind::Insight {
            insights.push(created.id);
        }
    }
    // замещения: системные рёбра `supersedes`, самоссылки `supersedes_id`/`superseded_by_id`
    for old in insights.iter().skip(40).take(5) {
        let e = new_entry(
            knowledge,
            EntryKind::Insight,
            format!("Замена {old}"),
            json!({"kind": "gotcha"}),
        );
        supersede_entry(db, *old, None, e, &a).await.unwrap();
    }
    // мягко удалённая запись (частичные индексы `deleted_at IS NULL`)
    delete_entry(db, insights[60], None, &a).await.unwrap();
    // показы и оценки записей
    let ids = insights.clone();
    db.write(move |tx| {
        for (n, id) in ids.iter().take(50).enumerate() {
            tx.execute(
                "INSERT INTO usage (ts, user_id, session_id, target_kind, target_id, event, source)
                 VALUES (?1, 'u', 's-m4', 'entry', ?2, ?3, 'prompt')",
                rusqlite::params![now, id, if n % 2 == 0 { "recall_shown" } else { "useful" }],
            )?;
        }
        Ok(())
    })
    .await
    .unwrap();
    marked.unwrap()
}

/// Review Focus 1: база M4 (0014) с записями, ревизиями, триггерами, чанками, рёбрами и
/// `usage` открывается через [`UserDbs::get`] и переходит на 0015 без потерь.
#[tokio::test]
async fn rebuild_migration_preserves_m4_db() {
    let dir = tmp_dir("m4-db");
    let dd = DataDir::open(&dir).unwrap();
    let user = Id::new();
    let path = dd.user_db(&user);
    let marked = {
        let db = Db::open(&path, &USER_MIGRATIONS[..14]).unwrap();
        fill_m4(&db).await
    };
    let (before, objects_before, edge_types) = {
        let c = conn(&path);
        assert_eq!(user_version(&c), 14);
        let types: Vec<String> = {
            let mut st = c
                .prepare("SELECT DISTINCT type FROM edges ORDER BY type")
                .unwrap();
            st.query_map([], |r| r.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        (
            table_counts(&c),
            schema_objects(&c, &["entries", "edges", "entry_revisions", "triggers"]),
            types,
        )
    };
    // все девять типов M4 есть в базе
    assert_eq!(edge_types.len(), 9, "{edge_types:?}");
    assert!(before["entries"] >= 300 && before["entry_revisions"] > before["entries"]);
    assert_eq!(before["triggers"], 50);
    assert!(before["chunks"] > 0 && before["usage"] == 50);

    let dbs = UserDbs::new(dd.users_dir());
    let db = dbs.get(user).await.unwrap();
    dbs.wait_rechunk(user).await;
    assert!(path.with_file_name(format!("{user}.db.bak-v14")).exists());
    let (after, objects_after, version, violations, fk, entries_sql) = db
        .read(|c| {
            let sql: String = c.query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'entries'",
                [],
                |r| r.get(0),
            )?;
            Ok((
                table_counts(c),
                schema_objects(c, &["entries", "edges", "entry_revisions", "triggers"]),
                user_version(c),
                fk_violations(c),
                fk_on(c),
                sql,
            ))
        })
        .await
        .unwrap();
    assert_eq!(version, USER_MIGRATIONS.len() as u32);
    assert!(version >= 15);
    assert_eq!(violations, 0, "foreign_key_check не пуст");
    assert!(fk, "foreign_keys выключены после миграции");
    assert!(entries_sql.contains("'question'"), "{entries_sql}");
    // временных таблиц пересборки не осталось, строки всех таблиц на месте
    assert!(!after.contains_key("entries_new") && !after.contains_key("edges_new"));
    // `meta` — отметки разовых задач открытия базы (`UserDbs::get`), а не данные
    for (t, n) in before.iter().filter(|(t, _)| t.as_str() != "meta") {
        assert_eq!(after.get(t), Some(n), "таблица {t}");
    }
    // индексы, автоиндексы и триггеры `entries`, `edges` и детей пересозданы все
    assert_eq!(objects_after, objects_before);
    // FTS по заголовку находит запись
    let hits = search_fts(
        &db,
        FtsQuery {
            text: "квазиуникальный".into(),
            space_ids: zam_store::list_spaces(&db)
                .await
                .unwrap()
                .into_iter()
                .map(|s| s.id)
                .collect(),
            path_prefix: None,
            top_k: 10,
            targets: TargetFilter::default(),
        },
    )
    .await
    .unwrap();
    assert!(
        hits.iter().any(|h| h.target == entry_ref(marked)),
        "{hits:?}"
    );
    // самоссылки замещения пережили пересборку
    let superseded: i64 = db
        .read(|c| {
            Ok(c.query_row(
                "SELECT count(*) FROM entries o JOIN entries n ON n.id = o.superseded_by_id
                 WHERE n.supersedes_id = o.id",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(superseded, 5);
}

/// CHECK 0015: статус из множества своего вида, NULL — только у changelog и release; тип
/// связи `contradicts` принимается.
#[tokio::test]
async fn entries_kind_status_check() {
    let dir = tmp_dir("kind-status");
    let dd = DataDir::open(&dir).unwrap();
    let db = UserDbs::new(dd.users_dir()).get(Id::new()).await.unwrap();
    let space = zam_store::list_spaces(&db).await.unwrap()[0].id;
    let insert = |kind: &'static str, status: Option<&'static str>| {
        let db = db.clone();
        async move {
            db.write(move |tx| {
                let now = zam_store::time::fmt_ts(Timestamp::now());
                tx.execute(
                    "INSERT INTO entries (id, space_id, kind, title, title_norm, status,
                                          author_kind, created_at, updated_at)
                     VALUES (?1, ?2, ?3, 't', 't', ?4, 'agent', ?5, ?5)",
                    rusqlite::params![Id::new(), space, kind, status, now],
                )?;
                Ok(())
            })
            .await
        }
    };
    let rejected = |r: Result<(), StoreError>| match r {
        Err(StoreError::Sqlite(e)) => e.to_string().contains("CHECK"),
        _ => false,
    };
    assert!(rejected(insert("term", Some("in_progress")).await));
    insert("task", Some("open")).await.unwrap();
    insert("task", Some("in_progress")).await.unwrap();
    insert("term", Some("active")).await.unwrap();
    insert("question", Some("answered")).await.unwrap();
    insert("insight", Some("revisit")).await.unwrap();
    insert("changelog", None).await.unwrap();
    assert!(rejected(insert("term", Some("revisit")).await));
    assert!(rejected(insert("question", Some("done")).await));
    assert!(rejected(insert("task", None).await));
    assert!(rejected(insert("changelog", Some("active")).await));
    assert!(rejected(insert("note", None).await));
    // `contradicts` — в CHECK `edges`
    db.write(move |tx| {
        let now = zam_store::time::fmt_ts(Timestamp::now());
        tx.execute(
            "INSERT INTO edges (id, space_id, src_kind, src_id, dst_kind, dst_id, type, origin,
                                created_at)
             VALUES (?1, ?2, 'entry', ?3, 'entry', ?4, 'contradicts', 'explicit', ?5)",
            rusqlite::params![Id::new(), space, Id::new(), Id::new(), now],
        )?;
        Ok(())
    })
    .await
    .unwrap();
}
