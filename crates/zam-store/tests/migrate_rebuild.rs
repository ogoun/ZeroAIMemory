//! Миграции с пересборкой таблиц (`Migration::rebuild`, спецификация M4.5 §1.1) и миграция
//! 0015 базы пользователя (виды `term`/`task`/`question`, связь `contradicts`, §1.2–1.3) на
//! базе M4 (0014) с данными — Review Focus 1 плана M4.5.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use rusqlite::Connection;
use serde_json::json;
use zam_core::{Id, TargetKind, TargetRef};
use zam_store::{
    Attribution, AuthorKind, Certainty, DataDir, Db, EdgeType, EntryKind, EntryPatch, EntryStatus,
    FtsQuery, Migration, NewEntry, NewReportFile, NewTrigger, SpaceKind, StoreError, TargetFilter,
    TriggerKind, USER_MIGRATIONS, UserDbs, attachment_link, create_entry, create_report,
    delete_entry, search_fts, supersede_entry, update_entry,
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
        solution: None,
    }
}

fn new_entry(space: Id, kind: EntryKind, title: String, data: serde_json::Value) -> NewEntry {
    NewEntry {
        refs: None,
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
        let mut e = new_entry(
            knowledge,
            EntryKind::Insight,
            format!("Замена {old}"),
            json!({"kind": "gotcha"}),
        );
        // база v14: таблицы `file_refs` ещё нет — ссылки не наследуются (пустой набор явно)
        e.refs = Some(Vec::new());
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
    let (after, objects_after, version, violations, fk, entries_sql, code_objects) = db
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
                schema_objects(
                    c,
                    &[
                        "file_refs",
                        "code_snapshots",
                        "code_files",
                        "code_symbols",
                        "code_refs",
                    ],
                ),
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
    // индексы, автоиндексы и триггеры `entries`, `edges` и детей пересозданы все; 0022 (M5.5)
    // добавляет только триггеры неизменности отчётов
    let added = [
        "entries: trigger entries_report_immutable",
        "entry_revisions: trigger entry_revisions_report_once",
    ];
    let objects_after: Vec<String> = objects_after
        .into_iter()
        .filter(|o| !added.contains(&o.as_str()))
        .collect();
    assert_eq!(objects_after, objects_before);
    // 0016 (M4.5) и 0023 (M5.6): ссылки на файлы и символы, таблицы карты кода — с нуля
    assert_eq!(
        code_objects,
        [
            "file_refs: index file_refs_owner",
            "file_refs: index file_refs_path",
            "code_snapshots: index sqlite_autoindex_code_snapshots_1",
            "code_files: index sqlite_autoindex_code_files_1",
            "code_symbols: index code_symbols_file",
            "code_symbols: index code_symbols_name",
            "code_symbols: index code_symbols_qual",
            "code_symbols: trigger code_symbols_fts_delete",
            "code_refs: index code_refs_name",
        ]
    );
    for t in [
        "code_snapshots",
        "code_files",
        "code_symbols",
        "code_symbols_fts",
        "code_refs",
    ] {
        assert_eq!(after.get(t), Some(&0), "таблица {t}");
    }
    assert!(!after.contains_key("code_file_edges"));
    // FTS по заголовку находит запись
    let hits = search_fts(
        &db,
        FtsQuery {
            alias_groups: Vec::new(),
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

// ---------- 0024 (M5.7): вид `idea` на базе M5.6 (0023) ----------

/// Объект схемы: `(тип, имя, таблица, SQL без лишних пробелов)`.
type MasterObject = (String, String, String, String);

/// Все объекты схемы `sqlite_master`.
fn master_objects(c: &Connection) -> BTreeSet<MasterObject> {
    let mut st = c
        .prepare("SELECT type, name, tbl_name, coalesce(sql, '') FROM sqlite_master")
        .unwrap();
    st.query_map([], |r| {
        let sql: String = r.get(3)?;
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            sql.split_whitespace().collect::<Vec<_>>().join(" "),
        ))
    })
    .unwrap()
    .collect::<Result<_, _>>()
    .unwrap()
}

/// Строка `entries`: id, вид, статус, `data`, `idempotency_key`, ревизия.
type EntryRow = (String, String, Option<String>, String, Option<String>, i64);

/// Строки `entries`, которые миграция обязана перенести как есть.
fn entry_rows(c: &Connection) -> Vec<EntryRow> {
    let mut st = c
        .prepare(
            "SELECT id, kind, status, data, idempotency_key, current_rev FROM entries ORDER BY id",
        )
        .unwrap();
    st.query_map([], |r| {
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
        ))
    })
    .unwrap()
    .collect::<Result<_, _>>()
    .unwrap()
}

/// Заполняет базу на 0023 записями всех видов: changelog, release, insight с триггером и
/// `Idempotency-Key`, term, task, question с вариантами, report с файлом; у части — ревизии.
/// Возвращает id отчёта и sha256 его файла.
async fn fill_m56(db: &Db) -> (Id, String) {
    let now = zam_store::time::fmt_ts(Timestamp::now());
    let (sol, kn, rules, hu) = (Id::new(), Id::new(), Id::new(), Id::new());
    db.write(move |tx| {
        for (id, kind, guid) in [
            (sol, "solution", Some("g-m57-mig")),
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
    let mut all = vec![
        new_entry(
            sol,
            EntryKind::Changelog,
            "Изменение".into(),
            json!({"kind": "fix"}),
        ),
        new_entry(
            sol,
            EntryKind::Release,
            "Релиз".into(),
            json!({"version": "0.0.9"}),
        ),
        new_entry(
            sol,
            EntryKind::Task,
            "Задача".into(),
            json!({"next_step": "сделать"}),
        ),
        new_entry(
            sol,
            EntryKind::Question,
            "Какую БД?".into(),
            json!({"options": [{"key": "a", "text": "SQLite"}, {"key": "b", "text": "Postgres"}]}),
        ),
        new_entry(
            kn,
            EntryKind::Term,
            "Выжимка".into(),
            json!({"definition": "Готовый ответ кэша", "aliases": ["digest"]}),
        ),
    ];
    let mut ins = new_entry(
        kn,
        EntryKind::Insight,
        "Инсайт".into(),
        json!({"kind": "gotcha"}),
    );
    ins.triggers = vec![NewTrigger {
        kind: TriggerKind::Keyword,
        pattern: "слово".into(),
        tools: None,
    }];
    ins.idempotency_key = Some("key-m57".into());
    all.push(ins);
    for (n, e) in all.into_iter().enumerate() {
        let (e, _) = create_entry(db, e, &a).await.unwrap();
        if n % 2 == 0 {
            update_entry(
                db,
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
        }
    }
    let sha = format!("{:08x}{}", 7, "cd".repeat(28));
    let file = NewReportFile {
        sha256: sha.clone(),
        filename: "loss.png".into(),
        mime: "image/png".into(),
        size: 100,
    };
    let mut rep = new_entry(sol, EntryKind::Report, "Отчёт".into(), json!({}));
    rep.body = format!(
        "## Итоги\n\n![loss.png]({})\n",
        attachment_link(&sha, "loss.png")
    );
    rep.certainty = None;
    rep.tags = Vec::new();
    let (rep, _) = create_report(db, rep, vec![file], &a).await.unwrap();
    (rep.id, sha)
}

async fn exec_sql(db: &Db, sql: String) -> Result<usize, StoreError> {
    db.write(move |tx| Ok(tx.execute(&sql, [])?)).await
}

fn immutable(r: &Result<usize, StoreError>) -> bool {
    matches!(r, Err(StoreError::Sqlite(e)) if e.to_string().contains("report-immutable"))
}

/// Пять триггеров отчёта (0022; три пересоздаются в 0024) срабатывают на прямой SQL: правка
/// отчёта, вторая ревизия, поздний файл, правка и удаление файла; файл на месте.
async fn assert_report_triggers(db: &Db, report: Id, sha: &str) {
    let r = exec_sql(
        db,
        format!("UPDATE entries SET title = 'x' WHERE kind = 'report' AND id = '{report}'"),
    )
    .await;
    assert!(immutable(&r), "entries_report_immutable: {r:?}");
    let r = exec_sql(
        db,
        format!(
            "INSERT INTO entry_revisions (entry_id, rev, title, body, data, tags, area, status,
                                          certainty, reason, ts, author_kind, user_id, token_id,
                                          session_id, machine)
             SELECT entry_id, 2, title, body, data, tags, area, status, certainty, reason, ts,
                    author_kind, user_id, token_id, session_id, machine
             FROM entry_revisions WHERE entry_id = '{report}' AND rev = 1"
        ),
    )
    .await;
    assert!(immutable(&r), "entry_revisions_report_once: {r:?}");
    let r = exec_sql(
        db,
        format!(
            "INSERT INTO entry_attachments (entry_id, blob_sha256, filename, mime, size, created_at)
             VALUES ('{report}', '{sha}', 'late.png', 'image/png', 100, '2030-01-01T00:00:00Z')"
        ),
    )
    .await;
    assert!(immutable(&r), "entry_attachments_report_only: {r:?}");
    let r = exec_sql(
        db,
        "UPDATE entry_attachments SET filename = 'x.png'".to_string(),
    )
    .await;
    assert!(immutable(&r), "entry_attachments_no_update: {r:?}");
    let r = exec_sql(db, "DELETE FROM entry_attachments".to_string()).await;
    assert!(immutable(&r), "entry_attachments_no_delete: {r:?}");
    assert_eq!(
        db.read(|c| Ok(count(c, "entry_attachments")))
            .await
            .unwrap(),
        1
    );
}

/// Review Focus 2 M5.7: пересборка `entries` в 0024 не теряет ни строк, ни защиты отчёта —
/// ни на `entries`, ни на дочерних таблицах. Объекты схемы до и после совпадают (кроме SQL
/// таблицы `entries`), все пять триггеров отчёта срабатывают на прямой SQL.
#[tokio::test]
async fn migrate_0024_keeps_report_triggers_and_rows() {
    let dir = tmp_dir("m56-db");
    let dd = DataDir::open(&dir).unwrap();
    let user = Id::new();
    let path = dd.user_db(&user);
    assert_eq!(USER_MIGRATIONS[22].version, 23);
    let (report, sha) = {
        let db = Db::open(&path, &USER_MIGRATIONS[..23]).unwrap();
        fill_m56(&db).await
    };
    let (counts_before, objects_before, rows_before) = {
        let c = conn(&path);
        assert_eq!(user_version(&c), 23);
        (table_counts(&c), master_objects(&c), entry_rows(&c))
    };
    assert!(rows_before.len() >= 7);
    assert!(
        rows_before.iter().any(|r| r.4.is_some()),
        "есть Idempotency-Key"
    );
    assert!(rows_before.iter().any(|r| r.1 == "report"));
    for t in [
        "entries_report_immutable",
        "entry_revisions_report_once",
        "entry_attachments_report_only",
        "entry_attachments_no_update",
        "entry_attachments_no_delete",
    ] {
        assert!(
            objects_before.iter().any(|o| o.0 == "trigger" && o.1 == t),
            "{t} до миграции"
        );
    }

    let dbs = UserDbs::new(dd.users_dir());
    let db = dbs.get(user).await.unwrap();
    dbs.wait_rechunk(user).await;
    assert!(path.with_file_name(format!("{user}.db.bak-v23")).exists());
    let (counts_after, objects_after, rows_after, version, violations, fk) = db
        .read(|c| {
            Ok((
                table_counts(c),
                master_objects(c),
                entry_rows(c),
                user_version(c),
                fk_violations(c),
                fk_on(c),
            ))
        })
        .await
        .unwrap();
    assert_eq!(version, USER_MIGRATIONS.len() as u32);
    assert!(version >= 24);
    assert_eq!(violations, 0, "foreign_key_check не пуст");
    assert!(fk, "foreign_keys выключены после миграции");
    // строки (в том числе Idempotency-Key) и счётчики всех таблиц
    assert_eq!(rows_after, rows_before);
    for (t, n) in counts_before.iter().filter(|(t, _)| t.as_str() != "meta") {
        assert_eq!(counts_after.get(t), Some(n), "таблица {t}");
    }
    assert!(!counts_after.contains_key("entries_new"));
    // объекты схемы: меняется только SQL таблицы `entries` (объекты таблиц 0025 сверяет
    // `migrate_0025_from_m57_db`: база открывается последней схемой)
    let is_entries = |o: &&MasterObject| o.0 == "table" && o.1 == "entries";
    let keep = |o: &&MasterObject| !is_entries(o) && !TABLES_0025.contains(&o.2.as_str());
    let before: Vec<&MasterObject> = objects_before.iter().filter(keep).collect();
    let after: Vec<&MasterObject> = objects_after.iter().filter(keep).collect();
    assert_eq!(after, before);
    let sql_of = |s: &BTreeSet<MasterObject>| s.iter().find(is_entries).unwrap().3.clone();
    let (sql_before, sql_after) = (sql_of(&objects_before), sql_of(&objects_after));
    assert!(!sql_before.contains("'idea'"));
    assert!(
        sql_after.contains("'idea'") && sql_after.contains("'declined'"),
        "{sql_after}"
    );

    assert_report_triggers(&db, report, &sha).await;
    // обычная запись правится, идея пишется
    exec_sql(
        &db,
        "UPDATE entries SET title = 'Задача 2' WHERE kind = 'task'".to_string(),
    )
    .await
    .unwrap();
    let space = zam_store::list_spaces(&db)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.kind == SpaceKind::Solution)
        .unwrap()
        .id;
    let mut idea = new_entry(
        space,
        EntryKind::Idea,
        "Идея".into(),
        json!({"origin": "human"}),
    );
    idea.certainty = None;
    let (idea, _) = create_entry(&db, idea, &agent()).await.unwrap();
    assert_eq!(idea.status, Some(EntryStatus::New));
}

// ---------- 0025 (M6): эпохи `session_seen`, holdout, учёт, снимки, `recv_count` ----------

/// Таблицы, которые меняет или создаёт 0025: их объекты сверяет `migrate_0025_from_m57_db`.
const TABLES_0025: [&str; 6] = [
    "sessions",
    "session_seen",
    "session_tokens",
    "code_refs",
    "tokens_unattributed",
    "session_snapshots",
];

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

fn decision(d: zam_store::IdeaDecision) -> zam_store::NewDecision {
    zam_store::NewDecision {
        decision: d,
        horizon: None,
        reason: None,
        by: zam_store::AnswerBy::Human,
        channel: zam_store::Channel::Webui,
        task_space: None,
        request_solution: None,
        defer_days: 30,
        expected_origin: None,
    }
}

/// Строка `session_seen` без эпохи: сессия, владелец, вид, цель, часть, ревизия, время.
type SeenRow = (String, String, String, String, String, i64, String);

/// Строки `session_seen`; `epoch` — со столбцом эпохи (после 0025).
fn seen_rows(c: &Connection, epoch: bool) -> Vec<(SeenRow, Option<i64>)> {
    let sql = if epoch {
        "SELECT session_id, owner_id, target_kind, target_id, part, rev, seen_at, epoch
         FROM session_seen ORDER BY 1, 2, 3, 4, 5"
    } else {
        "SELECT session_id, owner_id, target_kind, target_id, part, rev, seen_at, NULL
         FROM session_seen ORDER BY 1, 2, 3, 4, 5"
    };
    let mut st = c.prepare(sql).unwrap();
    st.query_map([], |r| {
        Ok((
            (
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ),
            r.get(7)?,
        ))
    })
    .unwrap()
    .collect::<Result<_, _>>()
    .unwrap()
}

/// Строки запроса целиком текстом (все столбцы через `|`).
fn all_rows(c: &Connection, sql: &str) -> Vec<String> {
    let mut st = c.prepare(sql).unwrap();
    let n = st.column_count();
    st.query_map([], |r| {
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let v: rusqlite::types::Value = r.get(i)?;
            out.push(format!("{v:?}"));
        }
        Ok(out.join("|"))
    })
    .unwrap()
    .collect::<Result<_, _>>()
    .unwrap()
}

/// Заполняет базу на 0024 тем, что появилось в M5.7: идея «показана» сессии, идея с
/// вопросом-предложением, принятая идея с задачей («начать»), отметки `session_seen` всех видов,
/// учёт сессии, сессия субагента, карта кода со ссылками. Возвращает id показанной идеи.
async fn fill_m57(db: &Db, owner: Id) -> Id {
    let space = zam_store::list_spaces(db)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.kind == SpaceKind::Solution)
        .unwrap()
        .id;
    let idea = |title: &str| {
        let mut e = new_entry(
            space,
            EntryKind::Idea,
            title.into(),
            json!({"origin": "human", "priority": "normal"}),
        );
        e.certainty = None;
        e
    };
    let (shown, _) = create_entry(db, idea("Показанная идея"), &human())
        .await
        .unwrap();
    let (proposed, _) = create_entry(db, idea("Предложенная идея"), &human())
        .await
        .unwrap();
    let (_, q) = zam_store::propose_idea(db, proposed.id, None, None, None, &agent())
        .await
        .unwrap();
    assert_eq!(q.kind, EntryKind::Question);
    let (started, _) = create_entry(db, idea("Начатая идея"), &human())
        .await
        .unwrap();
    let d = zam_store::decide_idea(
        db,
        started.id,
        decision(zam_store::IdeaDecision::Accept),
        &human(),
    )
    .await
    .unwrap();
    assert!(d.task.is_some(), "«начать» создаёт задачу");
    // отметки и учёт — SQL схемы 0024 (`mark_seen_with_tokens` пишет уже эпоху): показ идеи
    // подсказкой, цель целиком, чанк узла, блоб
    let s = zam_store::SessionTouch {
        id: "s-ideas".into(),
        agent: Some("claude".into()),
        ..zam_store::SessionTouch::default()
    };
    zam_store::touch_session(db, &s).await.unwrap();
    let marks = [
        (
            "entry",
            shown.id.to_string(),
            zam_store::RECALL_PART.to_string(),
        ),
        (
            "entry",
            proposed.id.to_string(),
            zam_store::WHOLE_TARGET.to_string(),
        ),
        ("node", Id::new().to_string(), "c1".to_string()),
        ("blob", "ab".repeat(32), "page:1".to_string()),
    ];
    let now = zam_store::time::fmt_ts(Timestamp::now());
    db.write(move |tx| {
        for (kind, id, part) in &marks {
            tx.execute(
                "INSERT INTO session_seen (session_id, owner_id, target_kind, target_id, part, rev,
                                           seen_at)
                 VALUES ('s-ideas', ?1, ?2, ?3, ?4, 1, ?5)",
                rusqlite::params![owner, kind, id, part, now],
            )?;
        }
        tx.execute(
            "INSERT INTO session_tokens (session_id, event, calls, tokens)
             VALUES ('s-ideas', 'session_start', 1, 900)",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let sub = zam_store::SessionTouch {
        id: "s-ideas:ag1".into(),
        parent_id: Some("s-ideas".into()),
        agent_type: Some("Explore".into()),
        ..zam_store::SessionTouch::default()
    };
    zam_store::touch_session(db, &sub).await.unwrap();
    // карта кода: снимок, файл, ссылки
    let now = zam_store::time::fmt_ts(Timestamp::now());
    db.write(move |tx| {
        tx.execute(
            "INSERT INTO code_snapshots (solution_guid, updated_at, files)
             VALUES ('g-m57-mig', ?1, 1)",
            [&now],
        )?;
        tx.execute(
            "INSERT INTO code_files (id, solution_guid, path, lang, content_hash, size,
                                     parse_state, updated_at)
             VALUES (1, 'g-m57-mig', 'src/lib.rs', 'rust', ?1, 10, 'ok', ?2)",
            ["0".repeat(64), now.clone()],
        )?;
        for (name, kind, n) in [
            ("Store", "type", 3),
            ("open", "call", 5),
            ("fmt", "import", 1),
        ] {
            tx.execute(
                "INSERT INTO code_refs (file_id, name, kind, count, first_line)
                 VALUES (1, ?1, ?2, ?3, 2)",
                rusqlite::params![name, kind, n],
            )?;
        }
        Ok(())
    })
    .await
    .unwrap();
    shown.id
}

/// Состояние базы для сверки миграции.
type Probe = (
    BTreeMap<String, i64>,
    BTreeSet<MasterObject>,
    Vec<(SeenRow, Option<i64>)>,
    [Vec<String>; 3],
);

/// Счётчики, объекты схемы, `session_seen` и строки `code_refs`, `sessions`, `session_tokens`.
fn probe(c: &Connection, epoch: bool) -> Probe {
    (
        table_counts(c),
        master_objects(c),
        seen_rows(c, epoch),
        [
            all_rows(c, "SELECT * FROM code_refs ORDER BY file_id, name, kind"),
            all_rows(c, "SELECT * FROM sessions ORDER BY id"),
            all_rows(c, "SELECT * FROM session_tokens ORDER BY session_id, event"),
        ],
    )
}

/// Спецификация M6 §13: база M5.7 (0024, получена из базы 0023 прогоном миграции и дополнена
/// идеями, вопросом-предложением, задачей «начать», отметками `session_seen`, учётом и картой
/// кода) открывается последней схемой: копия `bak-v24`; строки на месте (`session_seen` — в
/// эпохе 0, `recv_count = 0`, новые столбцы — по умолчанию, новые таблицы пусты); объекты
/// `sqlite_master` совпадают, кроме таблиц 0025; пять триггеров отчёта срабатывают;
/// `foreign_key_check` пуст, `integrity_check` ok; идея «показана» в своей сессии.
#[tokio::test]
async fn migrate_0025_from_m57_db() {
    let dir = tmp_dir("m57-db");
    let dd = DataDir::open(&dir).unwrap();
    let user = Id::new();
    assert_eq!(USER_MIGRATIONS[23].version, 24);
    assert_eq!(USER_MIGRATIONS[24].version, 25);
    assert!(USER_MIGRATIONS[24].rebuild);
    // База собирается вне `users/` и встаёт на место жёсткой ссылкой уже закрытой (как в
    // приёмке M5.7: конвейер документов сервера мигрировал бы её посреди заполнения).
    let fixture = dir.join("fixture-m57.db");
    let (report, sha) = {
        let db = Db::open(&fixture, &USER_MIGRATIONS[..23]).unwrap();
        fill_m56(&db).await
    };
    let shown = {
        let db = Db::open(&fixture, &USER_MIGRATIONS[..24]).unwrap();
        fill_m57(&db, user).await
    };
    let (counts_before, objects_before, seen_before, rows_before) = {
        let c = conn(&fixture);
        assert_eq!(user_version(&c), 24);
        c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode = DELETE;")
            .unwrap();
        probe(&c, false)
    };
    assert_eq!(seen_before.len(), 4);
    assert_eq!(
        rows_before.iter().map(Vec::len).collect::<Vec<_>>(),
        [3, 2, 1]
    );
    assert!(counts_before["entries"] >= 11, "{counts_before:?}");
    let path = dd.user_db(&user);
    std::fs::hard_link(&fixture, &path).unwrap();

    let dbs = UserDbs::new(dd.users_dir());
    let db = dbs.get(user).await.unwrap();
    dbs.wait_rechunk(user).await;
    assert!(path.with_file_name(format!("{user}.db.bak-v24")).exists());
    let ((counts_after, objects_after, seen_after, rows_after), checks) = db
        .read(|c| {
            let integrity: String = c.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
            Ok((
                probe(c, true),
                (user_version(c), fk_violations(c), fk_on(c), integrity),
            ))
        })
        .await
        .unwrap();
    let (version, violations, fk, integrity) = checks;
    assert_eq!(version, 25);
    assert_eq!(version, USER_MIGRATIONS.len() as u32);
    assert_eq!(violations, 0, "foreign_key_check не пуст");
    assert!(fk, "foreign_keys выключены после миграции");
    assert_eq!(integrity, "ok");

    // строки: `session_seen` — те же, в эпохе 0; добавленные столбцы — значения по умолчанию
    let want: Vec<(SeenRow, Option<i64>)> =
        seen_before.into_iter().map(|(r, _)| (r, Some(0))).collect();
    assert_eq!(seen_after, want);
    let with = |rows: &[String], tail: &str| -> Vec<String> {
        rows.iter().map(|r| format!("{r}|{tail}")).collect()
    };
    let [refs, sessions, tokens] = &rows_before;
    assert_eq!(rows_after[0], with(refs, "Integer(0)"), "recv_count = 0");
    assert_eq!(
        rows_after[1],
        with(sessions, "Integer(0)|Integer(0)"),
        "seen_epoch, holdout"
    );
    assert_eq!(rows_after[2], with(tokens, "Integer(0)"), "reread_calls");
    for (t, n) in counts_before.iter().filter(|(t, _)| t.as_str() != "meta") {
        assert_eq!(counts_after.get(t), Some(n), "таблица {t}");
    }
    assert_eq!(counts_after.get("tokens_unattributed"), Some(&0));
    assert_eq!(counts_after.get("session_snapshots"), Some(&0));
    assert!(!counts_after.keys().any(|t| t.ends_with("_new")));

    // объекты схемы: вне таблиц 0025 — без изменений; в них — ровно ожидаемое
    let outside = |o: &&MasterObject| !TABLES_0025.contains(&o.2.as_str());
    assert_eq!(
        objects_after.iter().filter(outside).collect::<Vec<_>>(),
        objects_before.iter().filter(outside).collect::<Vec<_>>()
    );
    let names = |s: &BTreeSet<MasterObject>| -> Vec<String> {
        s.iter()
            .filter(|o| !outside(o))
            .map(|o| format!("{} {}", o.0, o.1))
            .collect()
    };
    let new = [
        "index sqlite_autoindex_session_snapshots_1",
        "table session_snapshots",
        "table tokens_unattributed",
    ];
    let mut want: Vec<String> = names(&objects_before)
        .into_iter()
        .chain(new.iter().map(|s| s.to_string()))
        .collect();
    want.sort();
    assert_eq!(names(&objects_after), want);
    let sql = |name: &str| {
        objects_after
            .iter()
            .find(|o| o.0 == "table" && o.1 == name)
            .unwrap()
            .3
            .clone()
    };
    let seen_sql = sql("session_seen");
    assert!(
        seen_sql
            .contains("PRIMARY KEY (session_id, owner_id, target_kind, target_id, part, epoch)")
            && seen_sql.contains("REFERENCES sessions(id)")
            && seen_sql.contains("CHECK (target_kind IN ('node','entry','blob'))"),
        "{seen_sql}"
    );
    assert!(sql("code_refs").contains("recv_count INTEGER NOT NULL DEFAULT 0"));
    assert!(sql("tokens_unattributed").ends_with("WITHOUT ROWID"));

    assert_report_triggers(&db, report, &sha).await;

    // идея «показана» в своей сессии: показ идей — без эпохи (§7.2)
    let ids = zam_store::seen_entry_ids(&db, "s-ideas", user, zam_store::RECALL_PART)
        .await
        .unwrap();
    assert_eq!(ids, [shown]);
    assert!(zam_store::any_idea(&db, &ids).await.unwrap());
    // отметка эпохи 0 видна; повтор — та же строка (новый первичный ключ)
    let key = zam_store::SeenKey {
        owner_id: user,
        target: entry_ref(shown).into(),
        part: zam_store::RECALL_PART.into(),
    };
    let k = std::slice::from_ref(&key);
    assert_eq!(
        zam_store::seen_revs(&db, "s-ideas", 0, k).await.unwrap()[&key],
        1
    );
    let s = zam_store::SessionTouch {
        id: "s-ideas".into(),
        ..zam_store::SessionTouch::default()
    };
    let item = zam_store::SeenItem {
        key: key.clone(),
        rev: 2,
    };
    zam_store::mark_seen(&db, &s, std::slice::from_ref(&item))
        .await
        .unwrap();
    assert_eq!(db.read(|c| Ok(count(c, "session_seen"))).await.unwrap(), 4);
    assert_eq!(
        zam_store::seen_revs(&db, "s-ideas", 0, k).await.unwrap()[&key],
        2
    );
    assert_eq!(zam_store::session_epoch(&db, "s-ideas").await.unwrap(), 0);
}
