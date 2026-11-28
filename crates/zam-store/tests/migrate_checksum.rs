//! Контрольные суммы миграций, сверка схемы с эталоном и открытие без изменений (M7.4 §7.1):
//! таблица мигратора `zam_migrations`, `StoreError::MigrationChanged`, сторож CI
//! `migrations/SHA256SUMS`, `zam_store::inspect`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use zam_store::inspect::{ChecksumStatus, open_ro};
use zam_store::migrate::checksum;
use zam_store::{Db, Migration, SYSTEM_MIGRATIONS, StoreError, USER_MIGRATIONS};

fn tmp_dir(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("migrate-checksum")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn conn(path: &Path) -> Connection {
    Connection::open(path).unwrap()
}

fn user_version(c: &Connection) -> u32 {
    c.query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap()
}

/// Строки таблицы сумм: версия → (сумма, происхождение, время записи).
fn rows(c: &Connection) -> BTreeMap<u32, (String, String, String)> {
    let mut st = c
        .prepare("SELECT version, sha256, origin, recorded_at FROM zam_migrations")
        .unwrap();
    st.query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?, r.get(3)?))))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// База «до 0.0.17»: таблицы сумм нет (её создаёт только мигратор M7.4).
fn drop_checksums(path: &Path) {
    conn(path)
        .execute_batch("DROP TABLE zam_migrations")
        .unwrap();
}

/// Новая база: строка `applied` на каждую миграцию, сумма — от текста SQL, время — UTC; так и у
/// `system.db`.
#[tokio::test]
async fn checksum_recorded_on_apply() {
    let dir = tmp_dir("recorded");
    for (name, list) in [("u.db", USER_MIGRATIONS), ("s.db", SYSTEM_MIGRATIONS)] {
        let path = dir.join(name);
        drop(Db::open(&path, list).unwrap());
        let r = rows(&conn(&path));
        assert_eq!(r.len(), list.len(), "{name}");
        for m in list {
            let (sha, origin, at) = &r[&m.version];
            assert_eq!(sha, &checksum(m.sql), "{name} v{}", m.version);
            assert_eq!(origin, "applied", "{name} v{}", m.version);
            assert!(
                at.ends_with('Z') && at.parse::<jiff::Timestamp>().is_ok(),
                "{at}"
            );
        }
        // второе открытие ничего не дописывает
        let before = rows(&conn(&path));
        drop(Db::open(&path, list).unwrap());
        assert_eq!(rows(&conn(&path)), before, "{name}");
    }
}

/// База до 0.0.17 (без таблицы сумм): версии `≤ user_version` принимаются строками `adopted`
/// одной транзакцией, новые миграции — `applied`; база на последней версии — только `adopted` и
/// без копии `bak-v<N>`. `checksum_status` до открытия — `AdoptedPending`, после — `Ok`.
#[tokio::test]
async fn checksum_adopted_on_existing_db() {
    let dir = tmp_dir("adopted");
    let path = dir.join("u.db");
    drop(Db::open(&path, &USER_MIGRATIONS[..25]).unwrap());
    drop_checksums(&path);
    assert_eq!(
        open_ro(&path)
            .unwrap()
            .checksum_status(USER_MIGRATIONS)
            .unwrap(),
        ChecksumStatus::AdoptedPending
    );
    drop(Db::open(&path, USER_MIGRATIONS).unwrap());
    let r = rows(&conn(&path));
    assert_eq!(r.len(), USER_MIGRATIONS.len());
    for m in USER_MIGRATIONS {
        let want = if m.version <= 25 {
            "adopted"
        } else {
            "applied"
        };
        assert_eq!(r[&m.version].1, want, "v{}", m.version);
        assert_eq!(r[&m.version].0, checksum(m.sql), "v{}", m.version);
    }
    assert!(dir.join("u.db.bak-v25").exists());
    assert_eq!(
        open_ro(&path)
            .unwrap()
            .checksum_status(USER_MIGRATIONS)
            .unwrap(),
        ChecksumStatus::Ok
    );

    // база на последней версии без таблицы: только `adopted`, копии нет (миграций не было)
    let latest = dir.join("latest.db");
    drop(Db::open(&latest, USER_MIGRATIONS).unwrap());
    drop_checksums(&latest);
    drop(Db::open(&latest, USER_MIGRATIONS).unwrap());
    let r = rows(&conn(&latest));
    assert_eq!(r.len(), USER_MIGRATIONS.len());
    assert!(r.values().all(|(_, o, _)| o == "adopted"));
    assert!(
        !dir.join(format!("latest.db.bak-v{}", USER_MIGRATIONS.len()))
            .exists()
    );
}

/// Список миграций с изменённым текстом версии `v` (ещё один оператор в конце).
fn changed_list(v: usize) -> &'static [Migration] {
    let mut list = USER_MIGRATIONS.to_vec();
    let sql = format!("{}\nSELECT 1;\n", list[v - 1].sql);
    list[v - 1].sql = Box::leak(sql.into_boxed_str());
    Box::leak(list.into_boxed_slice())
}

/// Сумма строки ≠ сумме программы — `MigrationChanged {version, recorded, compiled}` (ошибка
/// постоянная), база не открывается и не меняется: ни копии `bak-v<N>`, ни следующих миграций.
/// Так ловится и повод — 0025, правленная после применения: база, обновлённая промежуточной
/// 0025 сборкой с суммами, итоговой сборкой не открывается.
#[tokio::test]
async fn checksum_changed_rejected() {
    let dir = tmp_dir("changed");
    let path = dir.join("u.db");
    drop(Db::open(&path, USER_MIGRATIONS).unwrap());
    let fake = "0".repeat(64);
    conn(&path)
        .execute(
            "UPDATE zam_migrations SET sha256 = ?1 WHERE version = 5",
            [&fake],
        )
        .unwrap();
    match Db::open(&path, USER_MIGRATIONS) {
        Err(e @ StoreError::MigrationChanged { .. }) => {
            assert!(e.is_permanent());
            let StoreError::MigrationChanged {
                version,
                recorded,
                compiled,
            } = &e
            else {
                unreachable!()
            };
            assert_eq!(*version, 5);
            assert_eq!(recorded, &fake);
            assert_eq!(compiled, &checksum(USER_MIGRATIONS[4].sql));
            let text = e.to_string();
            assert!(text.contains("миграция 5"), "{text}");
            assert!(text.contains("zam-server doctor"), "{text}");
        }
        other => panic!("ожидалась MigrationChanged: {other:?}"),
    }
    assert_eq!(
        open_ro(&path)
            .unwrap()
            .checksum_status(USER_MIGRATIONS)
            .unwrap(),
        ChecksumStatus::Changed(vec![5])
    );

    // база на 25 с изменённой (в программе) 0003: ни копии, ни миграций 26–27
    let mid = dir.join("mid.db");
    drop(Db::open(&mid, &USER_MIGRATIONS[..25]).unwrap());
    match Db::open(&mid, changed_list(3)) {
        Err(StoreError::MigrationChanged { version: 3, .. }) => {}
        other => panic!("ожидалась MigrationChanged v3: {other:?}"),
    }
    assert_eq!(user_version(&conn(&mid)), 25);
    assert!(!dir.join("mid.db.bak-v25").exists());

    // промежуточная 0025 (с суммами) → итоговая сборка
    let hist = dir.join("hist.db");
    drop(Db::open(&hist, historic_list("966cb85")).unwrap());
    match Db::open(&hist, USER_MIGRATIONS) {
        Err(StoreError::MigrationChanged { version: 25, .. }) => {}
        other => panic!("ожидалась MigrationChanged v25: {other:?}"),
    }
}

/// Сумма — после замены `\r\n` на `\n`: тот же файл с CRLF не считается изменённым.
#[tokio::test]
async fn checksum_crlf_normalized() {
    assert_eq!(checksum("a;\r\nb;\r\n"), checksum("a;\nb;\n"));
    assert_ne!(checksum("a;\nb;\n"), checksum("a;\nb;"));
    let dir = tmp_dir("crlf");
    let path = dir.join("u.db");
    drop(Db::open(&path, USER_MIGRATIONS).unwrap());
    let crlf: Vec<Migration> = USER_MIGRATIONS
        .iter()
        .map(|m| Migration {
            sql: Box::leak(m.sql.replace('\n', "\r\n").into_boxed_str()),
            ..*m
        })
        .collect();
    let crlf: &'static [Migration] = Box::leak(crlf.into_boxed_slice());
    drop(Db::open(&path, crlf).unwrap());
    assert_eq!(
        open_ro(&path).unwrap().checksum_status(crlf).unwrap(),
        ChecksumStatus::Ok
    );
}

/// Родитель и ребёнок с внешним ключом (версия 1), пересборка родителя (версия 2).
static REBUILD: &[Migration] = &[
    Migration {
        version: 1,
        sql: "CREATE TABLE parent (id INTEGER PRIMARY KEY, v TEXT NOT NULL);
              CREATE TABLE child (id INTEGER PRIMARY KEY, pid INTEGER REFERENCES parent (id));
              INSERT INTO parent VALUES (1, 'a'); INSERT INTO child VALUES (10, 1);",
        rebuild: false,
    },
    Migration {
        version: 2,
        sql: "CREATE TABLE parent_new (id INTEGER PRIMARY KEY, v TEXT NOT NULL, w TEXT);
              INSERT INTO parent_new (id, v) SELECT id, v FROM parent;
              DROP TABLE parent;
              ALTER TABLE parent_new RENAME TO parent;",
        rebuild: true,
    },
];

/// Пересборка оставляет висячую ссылку ребёнка — откат.
static REBUILD_BAD: &[Migration] = &[
    REBUILD[0],
    Migration {
        version: 2,
        sql: "CREATE TABLE parent_new (id INTEGER PRIMARY KEY, v TEXT NOT NULL);
              DROP TABLE parent;
              ALTER TABLE parent_new RENAME TO parent;",
        rebuild: true,
    },
];

/// Миграция с пересборкой пишет строку `applied` в своей транзакции (`rebuild_tx`): удачная — есть,
/// откаченная (нарушены внешние ключи) — нет. У настоящих миграций с пересборкой — тоже.
#[tokio::test]
async fn checksum_rebuild_migration_recorded() {
    let dir = tmp_dir("rebuild");
    let ok = dir.join("ok.db");
    let mut c = conn(&ok);
    c.pragma_update(None, "foreign_keys", true).unwrap();
    assert_eq!(zam_store::migrate::apply(&mut c, &ok, REBUILD).unwrap(), 2);
    let r = rows(&c);
    assert_eq!(r[&2].0, checksum(REBUILD[1].sql));
    assert_eq!(r[&2].1, "applied");

    let bad = dir.join("bad.db");
    let mut c = conn(&bad);
    c.pragma_update(None, "foreign_keys", true).unwrap();
    zam_store::migrate::apply(&mut c, &bad, &REBUILD_BAD[..1]).unwrap();
    assert!(matches!(
        zam_store::migrate::apply(&mut c, &bad, REBUILD_BAD),
        Err(StoreError::InvalidMigrations(_))
    ));
    assert_eq!(user_version(&c), 1);
    let r = rows(&c);
    assert!(!r.contains_key(&2), "{r:?}");

    let user = dir.join("u.db");
    drop(Db::open(&user, USER_MIGRATIONS).unwrap());
    let r = rows(&conn(&user));
    let rebuilt: Vec<u32> = USER_MIGRATIONS
        .iter()
        .filter(|m| m.rebuild)
        .map(|m| m.version)
        .collect();
    assert!(rebuilt.len() >= 3, "{rebuilt:?}");
    for v in rebuilt {
        assert_eq!(r[&v].1, "applied", "v{v}");
    }
}

/// Сторож CI: каждая строка `migrations/SHA256SUMS` (формат `sha256sum`) совпадает с файлом и с
/// миграцией программы (сумма после `\r\n` → `\n`), у выпущенных версий нет пропусков, файлы сверх
/// списка — только старшие версии (невыпущенные). Выпущенную миграцию не правят: новую в список
/// вносит итог волны, выпускающей её (ТЗ §6.3).
#[test]
fn migrations_released_frozen() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let sums = std::fs::read_to_string(root.join("SHA256SUMS")).expect("migrations/SHA256SUMS");
    let mut released: BTreeMap<&str, Vec<u32>> = BTreeMap::new();
    for (i, line) in sums.lines().enumerate() {
        let (hex, rel) = line
            .split_once("  ")
            .unwrap_or_else(|| panic!("строка {}: «{line}»", i + 1));
        assert_eq!(hex.len(), 64, "строка {}", i + 1);
        assert!(
            hex.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
        let (kind, file) = rel.split_once('/').unwrap();
        let list = match kind {
            "user" => USER_MIGRATIONS,
            "system" => SYSTEM_MIGRATIONS,
            other => panic!("каталог {other}"),
        };
        let version: u32 = file[..4].parse().unwrap();
        let text = std::fs::read_to_string(root.join(kind).join(file))
            .unwrap_or_else(|e| panic!("{rel}: {e}"));
        assert_eq!(
            checksum(&text),
            hex,
            "{rel}: файл выпущенной миграции изменён"
        );
        let m = list
            .get(version as usize - 1)
            .unwrap_or_else(|| panic!("{rel}: нет в программе"));
        assert_eq!(m.version, version);
        assert_eq!(checksum(m.sql), hex, "{rel}: миграция программы изменена");
        released.entry(kind).or_default().push(version);
    }
    for (kind, vs) in &released {
        let want: Vec<u32> = (1..=vs.len() as u32).collect();
        assert_eq!(vs, &want, "{kind}: пропуски или порядок в SHA256SUMS");
    }
    assert_eq!(released["system"].len(), 7);
    assert_eq!(released["user"].len(), 27);
    for kind in ["user", "system"] {
        let top = released[kind].len() as u32;
        for e in std::fs::read_dir(root.join(kind)).unwrap().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let v: u32 = name[..4].parse().unwrap();
            let listed = sums.contains(&format!("  {kind}/{name}"));
            assert!(
                listed || v > top,
                "{kind}/{name}: не в SHA256SUMS, а версия не старше выпущенных ({top})"
            );
        }
    }
}

/// Чистая база (и база на промежуточной версии) сверяется с эталоном своей версии без различий.
#[tokio::test]
async fn schema_diff_clean_fresh() {
    let dir = tmp_dir("diff-fresh");
    for (name, list) in [
        ("u.db", USER_MIGRATIONS),
        ("s.db", SYSTEM_MIGRATIONS),
        ("u25.db", &USER_MIGRATIONS[..25]),
    ] {
        let path = dir.join(name);
        drop(Db::open(&path, list).unwrap());
        let full = if name.starts_with('s') {
            SYSTEM_MIGRATIONS
        } else {
            USER_MIGRATIONS
        };
        let ro = open_ro(&path).unwrap();
        assert_eq!(ro.user_version().unwrap(), list.len() as u32);
        assert_eq!(
            ro.schema_diff(full).unwrap(),
            Vec::<String>::new(),
            "{name}"
        );
        assert!(ro.integrity_check().unwrap().is_empty(), "{name}");
        assert!(ro.quick_check().unwrap().is_empty(), "{name}");
        assert!(ro.fts_check().unwrap().is_empty(), "{name}");
    }
}

/// Имя первого объекта `kind` (`index` с SQL — не автоиндекс, `trigger`).
fn first_object(c: &Connection, kind: &str) -> String {
    c.query_row(
        "SELECT name FROM sqlite_master WHERE type = ?1 AND sql IS NOT NULL
           AND name NOT LIKE 'sqlite_%' ORDER BY name LIMIT 1",
        [kind],
        |r| r.get(0),
    )
    .unwrap()
}

/// Дрейф схемы виден: нет столбца, индекса, триггера; различий больше 20 — итог не длиннее 20
/// строк.
#[tokio::test]
async fn schema_diff_detects_missing_column_index_trigger() {
    let dir = tmp_dir("diff-drift");
    let path = dir.join("u.db");
    drop(Db::open(&path, USER_MIGRATIONS).unwrap());
    let (index, trigger) = {
        let c = conn(&path);
        let (i, t) = (first_object(&c, "index"), first_object(&c, "trigger"));
        c.execute_batch(&format!(
            "PRAGMA foreign_keys = OFF;
             ALTER TABLE nodes DROP COLUMN imported_from;
             DROP INDEX \"{i}\";
             DROP TRIGGER \"{t}\";"
        ))
        .unwrap();
        (i, t)
    };
    let diff = open_ro(&path)
        .unwrap()
        .schema_diff(USER_MIGRATIONS)
        .unwrap();
    let text = diff.join("\n");
    assert!(
        diff.iter()
            .any(|l| l.contains("nodes") && l.contains("imported_from")),
        "{text}"
    );
    assert!(diff.iter().any(|l| l.contains(&index)), "{text}");
    assert!(diff.iter().any(|l| l.contains(&trigger)), "{text}");
    assert_eq!(diff.len(), 3, "{text}");

    // много различий: не больше 20 строк, последняя — сколько ещё
    let many = dir.join("many.db");
    drop(Db::open(&many, USER_MIGRATIONS).unwrap());
    {
        let c = conn(&many);
        let names: Vec<String> = c
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'index' AND sql IS NOT NULL
                   AND name NOT LIKE 'sqlite_%'",
            )
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(names.len() > 25, "{}", names.len());
        for n in &names {
            c.execute_batch(&format!("DROP INDEX \"{n}\"")).unwrap();
        }
    }
    let diff = open_ro(&many)
        .unwrap()
        .schema_diff(USER_MIGRATIONS)
        .unwrap();
    assert_eq!(diff.len(), 20, "{diff:?}");
    assert!(diff[19].contains("ещё"), "{}", diff[19]);
}

/// Текст 0025 промежуточной версии (как в `migrate_rebuild.rs`): `61c5c14` — `trigger` вместо
/// `trigger_kind`, без `imported_from` и `node_idempotency`; `d00f46c` — без `imported_from` и
/// `node_idempotency`; `966cb85` — без `node_idempotency`.
fn historic_0025(rev: &str) -> &'static str {
    let full = USER_MIGRATIONS[24].sql;
    let mut sql = full.to_string();
    let start = full.find("CREATE TABLE node_idempotency (").unwrap();
    let rest = &full[start..];
    sql = sql.replace(&rest[..rest.find(';').unwrap() + 1], "");
    if rev != "966cb85" {
        sql = sql.replace(
            "ALTER TABLE nodes ADD COLUMN imported_from TEXT;\nALTER TABLE entries ADD COLUMN \
             imported_from TEXT;",
            "",
        );
    }
    if rev == "61c5c14" {
        sql = sql.replace(
            "  trigger_kind TEXT NOT NULL,         --",
            "  trigger TEXT NOT NULL,              --",
        );
    }
    assert_ne!(sql, full);
    Box::leak(sql.into_boxed_str())
}

fn historic_list(rev: &str) -> &'static [Migration] {
    let mut v = USER_MIGRATIONS[..25].to_vec();
    v[24].sql = historic_0025(rev);
    Box::leak(v.into_boxed_slice())
}

/// База промежуточной 0025 (сборка M6 — без таблицы сумм) после досборки схемы и миграций
/// 0026–0027 сверяется с эталоном без различий, суммы приняты.
#[tokio::test]
async fn schema_diff_clean_after_repair_0025() {
    let dir = tmp_dir("diff-repair");
    for rev in ["61c5c14", "d00f46c", "966cb85"] {
        let path = dir.join(format!("{rev}.db"));
        drop(Db::open(&path, historic_list(rev)).unwrap());
        drop_checksums(&path);
        assert!(
            !open_ro(&path)
                .unwrap()
                .schema_diff(USER_MIGRATIONS)
                .unwrap()
                .is_empty(),
            "{rev}: до досборки схема другая"
        );
        drop(Db::open(&path, USER_MIGRATIONS).unwrap());
        let ro = open_ro(&path).unwrap();
        assert_eq!(
            ro.schema_diff(USER_MIGRATIONS).unwrap(),
            Vec::<String>::new(),
            "{rev}"
        );
        assert_eq!(
            ro.checksum_status(USER_MIGRATIONS).unwrap(),
            ChecksumStatus::Ok
        );
    }
}

/// Таблица сумм не сравнивается: база с ней и без неё (до 0.0.17 или после отката на 0.0.16 —
/// прежняя сборка её не замечает) сверяется без различий.
#[tokio::test]
async fn schema_diff_ignores_checksum_table() {
    let dir = tmp_dir("diff-table");
    let with = dir.join("with.db");
    drop(Db::open(&with, USER_MIGRATIONS).unwrap());
    assert_eq!(rows(&conn(&with)).len(), USER_MIGRATIONS.len());
    let without = dir.join("without.db");
    drop(Db::open(&without, USER_MIGRATIONS).unwrap());
    drop_checksums(&without);
    for p in [&with, &without] {
        let ro = open_ro(p).unwrap();
        assert_eq!(
            ro.schema_diff(USER_MIGRATIONS).unwrap(),
            Vec::<String>::new()
        );
    }
}

/// `fts_check` (и `integrity_check`, с SQLite 3.44 — `xIntegrity` таблиц FTS5) видит
/// испорченный индекс FTS5 — только чтением.
#[tokio::test]
async fn fts_check_detects_damage() {
    let dir = tmp_dir("fts");
    let path = dir.join("u.db");
    drop(Db::open(&path, USER_MIGRATIONS).unwrap());
    {
        let c = conn(&path);
        for i in 0..50 {
            c.execute(
                "INSERT INTO vault_fts (item_id, title, open_text) VALUES (?1, ?2, 'gamma delta')",
                rusqlite::params![format!("id{i}"), format!("alpha beta {i}")],
            )
            .unwrap();
        }
        c.execute_batch("DELETE FROM vault_fts_data WHERE id NOT IN (1, 10)")
            .unwrap();
    }
    let ro = open_ro(&path).unwrap();
    let fts = ro.fts_check();
    assert!(
        fts.as_ref()
            .is_ok_and(|v| v.iter().any(|l| l.starts_with("vault_fts: "))),
        "{fts:?}"
    );
    let ic = ro.integrity_check();
    assert!(ic.as_ref().is_ok_and(|v| !v.is_empty()), "{ic:?}");
}

/// Замер на копии рабочей базы разработчика (вручную; M7.4, задача 1, шаг 5): `ZAM_DEV_DB` —
/// путь к **копии** `users/<id>.db` (с `-wal`/`-shm`; база меняется). Печатает время первого
/// открытия (миграции и принятие сумм), сверки схемы, чистого принятия сумм (таблица снята, база
/// на последней версии) и повторного открытия; сверка схемы должна быть пустой.
#[tokio::test]
#[ignore = "замер вручную: ZAM_DEV_DB=<копия базы пользователя>"]
async fn dev_db_first_open_measure() {
    use std::time::Instant;
    let Ok(p) = std::env::var("ZAM_DEV_DB") else {
        return;
    };
    let path = PathBuf::from(p);
    let v0 = open_ro(&path).unwrap().user_version().unwrap();
    let t = Instant::now();
    drop(Db::open(&path, USER_MIGRATIONS).unwrap());
    let first = t.elapsed();
    let (diff, diff_t, status) = {
        let ro = open_ro(&path).unwrap();
        let t = Instant::now();
        let diff = ro.schema_diff(USER_MIGRATIONS).unwrap();
        (
            diff,
            t.elapsed(),
            ro.checksum_status(USER_MIGRATIONS).unwrap(),
        )
    };
    drop_checksums(&path);
    // только принятие сумм: мигратор на базе последней версии без таблицы (одна транзакция)
    let adopt = {
        let mut c = conn(&path);
        let t = Instant::now();
        zam_store::migrate::apply(&mut c, &path, USER_MIGRATIONS).unwrap();
        t.elapsed()
    };
    assert_eq!(rows(&conn(&path)).len(), USER_MIGRATIONS.len());
    let t = Instant::now();
    drop(Db::open(&path, USER_MIGRATIONS).unwrap());
    let again = t.elapsed();
    eprintln!(
        "ЗАМЕР v{v0} → v{}: {} байт; первое открытие {first:?}; schema_diff {diff_t:?} {diff:?}; \
         суммы {status:?}; принятие сумм (apply) {adopt:?}; повторное открытие Db {again:?}",
        USER_MIGRATIONS.len(),
        std::fs::metadata(&path).unwrap().len()
    );
    assert!(diff.is_empty(), "{diff:?}");
    assert_eq!(status, ChecksumStatus::Ok);
}

/// Размер, время изменения файла и состав каталога.
fn file_state(dir: &Path, path: &Path) -> (u64, std::time::SystemTime, Vec<String>) {
    let md = std::fs::metadata(path).unwrap();
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    (md.len(), md.modified().unwrap(), names)
}

/// `open_ro` ничего не пишет: размер и `mtime` файла базы прежние, рядом не появилось файлов
/// (`-wal`, `-shm`), хотя прочитаны версия, суммы, сверка схемы и все проверки.
#[tokio::test]
async fn open_ro_no_writes() {
    let dir = tmp_dir("ro");
    let path = dir.join("u.db");
    drop(Db::open(&path, &USER_MIGRATIONS[..25]).unwrap());
    drop_checksums(&path);
    let before = file_state(&dir, &path);
    std::thread::sleep(std::time::Duration::from_millis(50));
    {
        let ro = open_ro(&path).unwrap();
        assert_eq!(ro.user_version().unwrap(), 25);
        assert_eq!(
            ro.checksum_status(USER_MIGRATIONS).unwrap(),
            ChecksumStatus::AdoptedPending
        );
        assert!(ro.schema_diff(USER_MIGRATIONS).unwrap().is_empty());
        assert!(ro.quick_check().unwrap().is_empty());
        assert!(ro.integrity_check().unwrap().is_empty());
        assert!(ro.fts_check().unwrap().is_empty());
    }
    assert_eq!(file_state(&dir, &path), before);
    // нет файла — ошибка, файл не создаётся
    assert!(open_ro(&dir.join("absent.db")).is_err());
    assert!(!dir.join("absent.db").exists());
}
