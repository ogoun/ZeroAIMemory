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

/// Новая база создаётся сразу на последней схеме (M5.6: 0023) с таблицами карты кода; файлы
/// карты привязаны к снимку решения каскадом (очистка карты не оставляет висячих строк).
#[tokio::test]
async fn fresh_db_has_code_map_schema() {
    let dd = datadir("code-map");
    let db = UserDbs::new(dd.users_dir()).get(Id::new()).await.unwrap();
    let (ver, tables, fk): (u32, Vec<String>, String) = db
        .read(|c| {
            let mut st = c.prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name LIKE 'code_%'
                 AND name NOT LIKE 'code_symbols_fts_%' ORDER BY name",
            )?;
            let tables = st
                .query_map([], |r| r.get(0))?
                .collect::<Result<Vec<String>, _>>()?;
            let fk: String = c.query_row(
                "SELECT \"table\" || ':' || on_delete FROM pragma_foreign_key_list('code_files')",
                [],
                |r| r.get(0),
            )?;
            Ok((
                c.query_row("PRAGMA user_version", [], |r| r.get(0))?,
                tables,
                fk,
            ))
        })
        .await
        .unwrap();
    assert_eq!(ver, zam_store::USER_MIGRATIONS.len() as u32);
    assert!(ver >= 23);
    assert_eq!(
        tables,
        [
            "code_files",
            "code_refs",
            "code_snapshots",
            "code_symbols",
            "code_symbols_fts"
        ]
    );
    assert_eq!(fk, "code_snapshots:CASCADE");
}

// ---------- 0027 (M7.3 §6 «Общее»): внутренние работы — только новые таблицы ----------

/// Семь таблиц миграции 0027.
const TABLES_0027: [&str; 7] = [
    "check_results",
    "check_runs",
    "probes",
    "proposals",
    "usage_opened",
    "work_calls",
    "works",
];

/// Объекты `sqlite_master` (тип, имя, таблица, SQL без лишних пробелов) по порядку.
fn master(c: &rusqlite::Connection) -> Vec<(String, String, String, String)> {
    let mut st = c
        .prepare(
            "SELECT type, name, tbl_name, coalesce(sql, '') FROM sqlite_master
             ORDER BY type, name",
        )
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

/// База версии 26 с показом в `usage` (строка прежней таблицы переживает миграцию).
fn v26_db(name: &str) -> (DataDir, Id, PathBuf) {
    let dd = datadir(name);
    let user = Id::new();
    let path = dd.user_db(&user);
    let mut c = rusqlite::Connection::open(&path).unwrap();
    let v = zam_store::migrate::apply(&mut c, &path, &zam_store::USER_MIGRATIONS[..26]).unwrap();
    assert_eq!(v, 26);
    c.execute(
        "INSERT INTO usage (ts, user_id, session_id, target_kind, target_id, event, source)
         VALUES ('2026-11-01T00:00:00.000000Z', ?1, 's', 'entry', ?2, 'recall_shown', 'prompt')",
        rusqlite::params![user, Id::new()],
    )
    .unwrap();
    (dd, user, path)
}

/// M7.3 §6 «Общее»: база версии 26 открывается схемой 0027 — копия `bak-v26`, версия 27, семь
/// новых таблиц есть, а таблицы, индексы и триггеры прежних таблиц (`sqlite_master`, кроме
/// новых) не изменились; строки прежних таблиц на месте.
#[tokio::test]
async fn migrate_v26_to_v27_new_tables_only() {
    let (dd, user, path) = v26_db("mig-v27");
    let before = master(&rusqlite::Connection::open(&path).unwrap());
    assert!(before.iter().all(|o| !TABLES_0027.contains(&o.2.as_str())));
    let db = UserDbs::new(dd.users_dir()).get(user).await.unwrap();
    assert!(path.with_file_name(format!("{user}.db.bak-v26")).exists());
    let (ver, after, usage): (u32, _, i64) = db
        .read(|c| {
            Ok((
                c.query_row("PRAGMA user_version", [], |r| r.get(0))?,
                master(c),
                c.query_row("SELECT count(*) FROM usage", [], |r| r.get(0))?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(ver, 27);
    assert_eq!(ver, zam_store::USER_MIGRATIONS.len() as u32);
    assert!(
        !zam_store::USER_MIGRATIONS[26].rebuild,
        "0027 — без пересборки"
    );
    assert_eq!(usage, 1);
    let tables: Vec<&str> = after
        .iter()
        .filter(|o| o.0 == "table" && TABLES_0027.contains(&o.1.as_str()))
        .map(|o| o.1.as_str())
        .collect();
    assert_eq!(tables, TABLES_0027);
    let old: Vec<_> = after
        .iter()
        .filter(|o| !TABLES_0027.contains(&o.2.as_str()))
        .cloned()
        .collect();
    assert_eq!(old, before, "объекты прежних таблиц не изменились");
    // индексы своих запросов (план M7.3, задача 1)
    for (index, table) in [
        ("works_open", "works"),
        ("works_night", "works"),
        ("works_finished", "works"),
        ("work_calls_ts", "work_calls"),
        ("work_calls_run", "work_calls"),
        ("probes_active", "probes"),
        ("check_runs_night", "check_runs"),
        ("proposals_open", "proposals"),
        ("proposals_dedup", "proposals"),
        ("usage_opened_target", "usage_opened"),
    ] {
        assert!(
            after
                .iter()
                .any(|o| o.0 == "index" && o.1 == index && o.2 == table),
            "нет индекса {index}"
        );
    }
    // внешних ключей на `entries` и `sessions` нет (§6 «Общее», §6.6 п. 1)
    let fks: Vec<String> = db
        .read(|c| {
            let mut out = Vec::new();
            for t in TABLES_0027 {
                let mut st = c.prepare(&format!(
                    "SELECT \"table\" FROM pragma_foreign_key_list('{t}')"
                ))?;
                for r in st.query_map([], |r| r.get::<_, String>(0))? {
                    out.push(format!("{t}→{}", r?));
                }
            }
            Ok(out)
        })
        .await
        .unwrap();
    assert!(
        fks.iter()
            .all(|f| !f.ends_with("→entries") && !f.ends_with("→sessions")),
        "{fks:?}"
    );
    // ключи таблиц (§6.5 п. 7, §6.6 п. 1)
    let sql = |name: &str| {
        after
            .iter()
            .find(|o| o.0 == "table" && o.1 == name)
            .unwrap()
            .3
            .clone()
    };
    assert!(
        sql("check_results").contains("PRIMARY KEY (run_id, probe_id, space_id)"),
        "{}",
        sql("check_results")
    );
    assert!(
        sql("usage_opened").contains("PRIMARY KEY (session_id, target_kind, target_id)"),
        "{}",
        sql("usage_opened")
    );
    assert!(sql("check_runs").contains("trigger_kind"));
    assert!(sql("works").contains("result_key TEXT"));
    // у вида работы и типа предложения CHECK нет: значения проверяет код (§6 «Общее»)
    assert!(!sql("works").contains("CHECK (kind"), "{}", sql("works"));
    assert!(
        !sql("proposals").contains("CHECK (type"),
        "{}",
        sql("proposals")
    );
}

/// M7.3 §6.6 п. 1: `usage` не пересобирается — индекс `usage_target` после 0027 на месте, тем же
/// SQL, что в 0013.
#[tokio::test]
async fn usage_target_index_kept() {
    let (dd, user, _) = v26_db("usage-target");
    let db = UserDbs::new(dd.users_dir()).get(user).await.unwrap();
    let sql: String = db
        .read(|c| {
            Ok(c.query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = 'usage_target'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(
        sql,
        "CREATE INDEX usage_target ON usage(target_kind, target_id, event)"
    );
}

/// M7.3 §6.6 п. 3: строка `meta.usage_opened_since` — момент применения миграции в формате
/// хранилища (шесть знаков долей) и разбирается как время.
#[tokio::test]
async fn meta_usage_opened_since_set() {
    let t0 = jiff::Timestamp::now();
    let (dd, user, _) = v26_db("opened-since");
    let db = UserDbs::new(dd.users_dir()).get(user).await.unwrap();
    let raw: String = db
        .read(|c| {
            Ok(c.query_row(
                "SELECT value FROM meta WHERE key = 'usage_opened_since'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    let ts = zam_store::time::parse_ts(&raw).unwrap();
    assert_eq!(zam_store::time::fmt_ts(ts), raw, "формат хранилища");
    assert_eq!(zam_store::usage_opened_since(&db).await.unwrap(), Some(ts));
    // момент применения: не раньше начала теста (с точностью до секунды strftime)
    assert!(ts.as_second() >= t0.as_second(), "{raw}");
    // новая база получает строку так же
    let fresh = UserDbs::new(dd.users_dir()).get(Id::new()).await.unwrap();
    assert!(
        zam_store::usage_opened_since(&fresh)
            .await
            .unwrap()
            .is_some()
    );
}

/// M7.3 §6.6 пп. 1–2: одно открытие на сессию и цель держит первичный ключ — повтор той же
/// сессии и цели `false`, строка одна; другая сессия или цель — новая строка.
#[tokio::test]
async fn record_opened_once_per_session_target() {
    use zam_core::{TargetKind, TargetRef};
    use zam_store::{OpenedEvent, UsageSource, opened_counts_many, record_opened};
    let dd = datadir("opened-once");
    let user = Id::new();
    let db = UserDbs::new(dd.users_dir()).get(user).await.unwrap();
    let (a, b) = (Id::new(), Id::new());
    let ev = |session: &str, id: Id| OpenedEvent {
        ts: zam_store::time::now(),
        user_id: user,
        session_id: session.into(),
        target: TargetRef {
            kind: TargetKind::Entry,
            id,
        },
        source: UsageSource::Prompt,
    };
    assert!(record_opened(&db, &ev("s1", a)).await.unwrap());
    assert!(!record_opened(&db, &ev("s1", a)).await.unwrap());
    let rows: i64 = db
        .read(|c| Ok(c.query_row("SELECT count(*) FROM usage_opened", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(rows, 1);
    assert!(record_opened(&db, &ev("s2", a)).await.unwrap());
    assert!(record_opened(&db, &ev("s1", b)).await.unwrap());
    let n = opened_counts_many(&db, TargetKind::Entry, &[a, b, Id::new()])
        .await
        .unwrap();
    assert_eq!((n.get(&a), n.get(&b), n.len()), (Some(&2), Some(&1), 2));
    // вид цели в ключе: узел с тем же id — другая цель
    assert!(
        opened_counts_many(&db, TargetKind::Node, &[a])
            .await
            .unwrap()
            .is_empty()
    );
    // id сессии проверяется, как у `usage`
    assert!(record_opened(&db, &ev("", a)).await.is_err());
}
