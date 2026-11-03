//! Карта кода в хранилище (M5.6, задача 3): миграция 0023, таблицы `code_*`, ссылки на символы
//! в `file_refs` и правило устарелости по символу (спецификация M5.6 §6, §8, §17 п. 4 и п. 12).

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use rusqlite::Connection;
use serde_json::json;
use zam_code::{
    FileUpload, Lang, MISSING_HASH, ManifestItem, ParseState, RefAgg, RefKind, SkipReason, Symbol,
    SymbolHash, SymbolKind,
};
use zam_core::Id;
use zam_store::code::{
    RankState, UploadMeta, code_clear, code_impact, code_map_rows, code_put_files,
    code_rank_inputs, code_status, code_store_ranks, code_symbol, code_symbols_named,
    code_symbols_search, code_sync_manifest,
};
use zam_store::file_refs::{ChangedFile, SYMBOL_CHANGED, SYMBOL_REMOVED, note_refs};
use zam_store::{
    Attribution, AuthorKind, DataDir, Db, EntryKind, FileRef, FtsQuery, NewEntry, NewNote,
    NodePath, SpaceKind, TargetFilter, USER_MIGRATIONS, UserDbs, create_entry, create_note,
    create_solution, files_changed, files_verified, list_spaces, refs_index, refs_to_path,
    search_fts, space_of_kind,
};

const G: &str = "g-code-map";

fn datadir(name: &str) -> DataDir {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("code-map-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    DataDir::open(&dir).unwrap()
}

/// База пользователя с решением `G`: `(db, пространство решения)`.
async fn setup(name: &str) -> (Db, Id) {
    let dd = datadir(name);
    let db = UserDbs::new(dd.users_dir()).get(Id::new()).await.unwrap();
    let (s, _) = create_solution(&db, G, "Решение").await.unwrap();
    (db, s.id)
}

fn h(c: char) -> String {
    c.to_string().repeat(64)
}

fn bh(c: char) -> String {
    c.to_string().repeat(16)
}

fn meta(machine: &str) -> UploadMeta {
    UploadMeta {
        commit: Some("abc1234".into()),
        branch: Some("main".into()),
        machine: Some(machine.into()),
        user_id: Id::new(),
        now: Timestamp::now(),
    }
}

fn sym(name: &str, qual: &str, body: &str, line: u32) -> Symbol {
    Symbol {
        name: name.into(),
        qual: qual.into(),
        kind: SymbolKind::Function,
        line,
        end_line: line + 2,
        sig: Some(format!("fn {name}()")),
        body_hash: body.into(),
        exported: Some(true),
    }
}

fn rf(name: &str, count: u32) -> RefAgg {
    RefAgg {
        name: name.into(),
        kind: RefKind::Call,
        count,
        first_line: 1,
    }
}

fn upload(path: &str, hash: &str, symbols: Vec<Symbol>, refs: Vec<RefAgg>) -> FileUpload {
    FileUpload {
        path: path.into(),
        lang: zam_code::lang_of(path).unwrap_or(Lang::Rust),
        hash: hash.into(),
        git_oid: None,
        size: 100,
        parse_state: ParseState::Ok,
        skip_reason: None,
        symbols,
        refs,
    }
}

fn agent() -> Attribution {
    Attribution {
        solution: Some(G.into()),
        ..Attribution::new(AuthorKind::Agent)
    }
}

fn fref(path: &str, symbol: Option<&str>, hash: Option<String>, sh: Option<String>) -> FileRef {
    FileRef {
        path: path.into(),
        symbol: symbol.map(str::to_string),
        hash,
        symbol_hash: sh,
        commit: None,
    }
}

/// Запись журнала со ссылками `refs` в пространстве `space`.
async fn entry_with(db: &Db, space: Id, title: &str, refs: Vec<FileRef>) -> Id {
    let e = NewEntry {
        refs: Some(refs),
        space_id: space,
        kind: EntryKind::Changelog,
        title: title.into(),
        body: "Тело.".into(),
        data: json!({"kind": "fix"}),
        area: None,
        tags: Vec::new(),
        status: None,
        certainty: None,
        idempotency_key: None,
        triggers: Vec::new(),
        links: Vec::new(),
    };
    create_entry(db, e, &agent()).await.unwrap().0.id
}

/// Состояние ссылки `(path, symbol)`: `(state, stale_reason, content_hash, symbol_hash)`.
type RefRow = (String, Option<String>, Option<String>, Option<String>);

async fn ref_row(db: &Db, path: &str, symbol: Option<&str>) -> RefRow {
    let (path, symbol) = (path.to_string(), symbol.unwrap_or("").to_string());
    db.read(move |c| {
        Ok(c.query_row(
            "SELECT state, stale_reason, content_hash, symbol_hash FROM file_refs
             WHERE path = ?1 AND coalesce(symbol, '') = ?2",
            [&path, &symbol],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?)
    })
    .await
    .unwrap()
}

async fn count(db: &Db, sql: &'static str) -> i64 {
    db.read(move |c| Ok(c.query_row(sql, [], |r| r.get(0))?))
        .await
        .unwrap()
}

fn changed(path: &str, hash: &str, symbols: Option<Vec<(&str, &str)>>) -> ChangedFile {
    ChangedFile {
        path: path.into(),
        hash: hash.into(),
        symbols: symbols.map(|v| {
            v.into_iter()
                .map(|(q, b)| SymbolHash {
                    qual: q.into(),
                    body_hash: b.into(),
                })
                .collect()
        }),
    }
}

// ---------- миграция 0023 ----------

fn conn(path: &Path) -> Connection {
    let c = Connection::open(path).unwrap();
    c.pragma_update(None, "foreign_keys", true).unwrap();
    c
}

/// §17 п. 12: база M5.5 (0022) с 10 ссылками переходит на 0023 — ссылки на месте, `symbol`
/// NULL, внешние ключи целы, уникальный индекс владельца — по выражению с символом.
#[tokio::test]
async fn migration_0023_keeps_file_refs() {
    let dd = datadir("migration");
    let user = Id::new();
    let path = dd.user_db(&user);
    assert_eq!(USER_MIGRATIONS[21].version, 22);
    {
        let db = Db::open(&path, &USER_MIGRATIONS[..22]).unwrap();
        db.write(|tx| {
            let now = zam_store::time::fmt_ts(Timestamp::now());
            for i in 0..10 {
                tx.execute(
                    "INSERT INTO file_refs (owner_kind, owner_id, solution_guid, path,
                                            content_hash, recorded_at, state)
                     VALUES ('entry', ?1, 'g-old', ?2, ?3, ?4, 'ok')",
                    rusqlite::params![Id::new(), format!("src/f{i}.rs"), "a".repeat(64), now],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    }
    assert_eq!(
        conn(&path)
            .query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        22
    );
    let dbs = UserDbs::new(dd.users_dir());
    let db = dbs.get(user).await.unwrap();
    dbs.wait_rechunk(user).await;
    let (n, nulls, violations, version, idx) = db
        .read(|c| {
            let n: i64 = c.query_row("SELECT count(*) FROM file_refs", [], |r| r.get(0))?;
            let nulls: i64 = c.query_row(
                "SELECT count(*) FROM file_refs WHERE symbol IS NULL AND symbol_hash IS NULL",
                [],
                |r| r.get(0),
            )?;
            let mut st = c.prepare("PRAGMA foreign_key_check")?;
            let violations = st.query_map([], |_| Ok(()))?.count();
            let v: u32 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
            let idx: String = c.query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = 'file_refs_owner'",
                [],
                |r| r.get(0),
            )?;
            Ok((n, nulls, violations, v, idx))
        })
        .await
        .unwrap();
    assert_eq!(n, 10);
    assert_eq!(nulls, 10);
    assert_eq!(violations, 0, "foreign_key_check не пуст");
    assert_eq!(version, USER_MIGRATIONS.len() as u32);
    assert!(version >= 23);
    assert!(idx.contains("UNIQUE"), "{idx}");
    assert!(idx.contains("COALESCE(symbol, '')"), "{idx}");
    for t in [
        "code_snapshots",
        "code_files",
        "code_symbols",
        "code_symbols_fts",
        "code_refs",
    ] {
        let t = t.to_string();
        let k: i64 = db
            .read(move |c| Ok(c.query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get(0))?))
            .await
            .unwrap();
        assert_eq!(k, 0);
    }
}

/// Один путь — файловая ссылка и ссылки на два символа; повтор `(путь, символ)` — одна строка;
/// уникальный индекс не пускает дубль файловой ссылки.
#[tokio::test]
async fn file_ref_same_path_two_symbols_and_file_unique() {
    let (db, sol) = setup("unique").await;
    let id = entry_with(
        &db,
        sol,
        "Три ссылки",
        vec![
            fref("a.rs", None, Some(h('1')), None),
            fref("a.rs", Some("foo"), Some(h('1')), Some(bh('f'))),
            fref("a.rs", Some("bar"), None, None),
            fref("./a.rs", Some("foo"), None, None),
        ],
    )
    .await;
    assert_eq!(count(&db, "SELECT count(*) FROM file_refs").await, 3);
    let dup = db
        .write(move |tx| {
            tx.execute(
                "INSERT INTO file_refs (owner_kind, owner_id, path, recorded_at)
                 VALUES ('entry', ?1, 'a.rs', 'now')",
                [id],
            )?;
            Ok(())
        })
        .await;
    assert!(dup.is_err(), "дубль файловой ссылки принят");
    let dup_sym = db
        .write(move |tx| {
            tx.execute(
                "INSERT INTO file_refs (owner_kind, owner_id, path, symbol, recorded_at)
                 VALUES ('entry', ?1, 'a.rs', 'foo', 'now')",
                [id],
            )?;
            Ok(())
        })
        .await;
    assert!(dup_sym.is_err(), "дубль символьной ссылки принят");
    // первая из повторов побеждает: хэш символа записан
    assert_eq!(ref_row(&db, "a.rs", Some("foo")).await.3, Some(bh('f')));
}

// ---------- манифест и загрузка ----------

/// 3 файла → `need` 3; повтор с тем же хэшем → пусто; без одного → `removed` 1, его символы,
/// ссылки карты и строки FTS удалены каскадом.
#[tokio::test]
async fn manifest_need_and_removed() {
    let (db, _) = setup("manifest").await;
    let m = meta("box");
    let man = |paths: &[(&str, char)]| -> Vec<ManifestItem> {
        paths
            .iter()
            .map(|(p, c)| ManifestItem {
                path: (*p).into(),
                hash: h(*c),
            })
            .collect()
    };
    let three = man(&[("a.rs", '1'), ("b.rs", '2'), ("c.rs", '3')]);
    let out = code_sync_manifest(&db, G, &m, &three).await.unwrap();
    let mut need = out.need.clone();
    need.sort();
    assert_eq!(need, ["a.rs", "b.rs", "c.rs"]);
    assert_eq!(out.removed, 0);
    let files = vec![
        upload(
            "a.rs",
            &h('1'),
            vec![sym("foo", "foo", &bh('a'), 1)],
            vec![rf("bar", 2)],
        ),
        upload(
            "b.rs",
            &h('2'),
            vec![sym("bar", "bar", &bh('b'), 1)],
            vec![],
        ),
        upload(
            "c.rs",
            &h('3'),
            vec![sym("baz", "baz", &bh('c'), 1)],
            vec![rf("foo", 1)],
        ),
    ];
    code_put_files(&db, G, &m, files, &[]).await.unwrap();
    let again = code_sync_manifest(&db, G, &m, &three).await.unwrap();
    assert!(again.need.is_empty(), "{:?}", again.need);
    assert_eq!(again.removed, 0);
    // другой хэш — нужен снова
    let changed = man(&[("a.rs", '9'), ("b.rs", '2'), ("c.rs", '3')]);
    assert_eq!(
        code_sync_manifest(&db, G, &m, &changed).await.unwrap().need,
        ["a.rs"]
    );
    let two = man(&[("a.rs", '1'), ("b.rs", '2')]);
    let out = code_sync_manifest(&db, G, &m, &two).await.unwrap();
    assert_eq!(out.removed, 1);
    assert_ne!(out.etag, again.etag);
    assert_eq!(count(&db, "SELECT count(*) FROM code_files").await, 2);
    assert_eq!(
        count(&db, "SELECT count(*) FROM code_symbols WHERE name = 'baz'").await,
        0
    );
    assert_eq!(
        count(&db, "SELECT count(*) FROM code_refs WHERE name = 'foo'").await,
        0
    );
    assert_eq!(count(&db, "SELECT count(*) FROM code_symbols_fts").await, 2);
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM code_symbols_fts WHERE code_symbols_fts MATCH 'baz'"
        )
        .await,
        0
    );
    let st = code_status(&db, G).await.unwrap().unwrap();
    assert_eq!(st.snapshot.files, 2);
    assert_eq!(st.snapshot.symbols, 2);
    assert_eq!(st.snapshot.rank_state, RankState::Pending);
}

/// Повторная загрузка файла заменяет символы, ссылки и строки FTS, `id` файла сохраняется.
#[tokio::test]
async fn put_files_replaces_symbols_keeps_file_id() {
    let (db, _) = setup("replace").await;
    let m = meta("box");
    let first = vec![upload(
        "a.rs",
        &h('1'),
        vec![
            sym("foo", "foo", &bh('a'), 1),
            sym("old", "old", &bh('o'), 5),
        ],
        vec![rf("x", 1)],
    )];
    code_put_files(&db, G, &m, first, &[]).await.unwrap();
    let id1 = count(&db, "SELECT id FROM code_files WHERE path = 'a.rs'").await;
    let second = vec![upload(
        "a.rs",
        &h('2'),
        vec![
            sym("foo", "foo", &bh('b'), 1),
            sym("new", "new", &bh('n'), 9),
        ],
        vec![rf("y", 3)],
    )];
    let out = code_put_files(&db, G, &m, second, &[]).await.unwrap();
    assert_eq!(out.accepted, 1);
    let id2 = count(&db, "SELECT id FROM code_files WHERE path = 'a.rs'").await;
    assert_eq!(id1, id2);
    assert_eq!(count(&db, "SELECT count(*) FROM code_symbols").await, 2);
    assert_eq!(
        count(&db, "SELECT count(*) FROM code_symbols WHERE name = 'old'").await,
        0
    );
    assert_eq!(
        count(&db, "SELECT count(*) FROM code_refs WHERE name = 'y'").await,
        1
    );
    assert_eq!(count(&db, "SELECT count(*) FROM code_refs").await, 1);
    assert_eq!(count(&db, "SELECT count(*) FROM code_symbols_fts").await, 2);
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM code_symbols_fts WHERE code_symbols_fts MATCH 'old'"
        )
        .await,
        0
    );
    assert_eq!(
        count(&db, "SELECT content_hash = '2222222222222222222222222222222222222222222222222222222222222222' FROM code_files").await,
        1
    );
}

/// Review Focus 4: две загрузки одного пути с разных машин — одна строка файла, символы второй,
/// осиротевших символов нет.
#[tokio::test]
async fn put_files_same_path_twice_last_wins_no_orphans() {
    let (db, _) = setup("twice").await;
    let a = vec![upload(
        "a.rs",
        &h('1'),
        vec![sym("fa", "fa", &bh('a'), 1)],
        vec![],
    )];
    let b = vec![upload(
        "a.rs",
        &h('2'),
        vec![sym("fb", "fb", &bh('b'), 1), sym("gb", "gb", &bh('c'), 4)],
        vec![],
    )];
    let (d1, d2) = (db.clone(), db.clone());
    let (win, linux) = (meta("win"), meta("linux"));
    let (r1, r2) = tokio::join!(
        code_put_files(&d1, G, &win, a, &[]),
        code_put_files(&d2, G, &linux, b.clone(), &[])
    );
    r1.unwrap();
    r2.unwrap();
    // порядок двух конкурентных загрузок не задан — повторяем вторую последней
    code_put_files(&db, G, &meta("linux"), b, &[])
        .await
        .unwrap();
    assert_eq!(count(&db, "SELECT count(*) FROM code_files").await, 1);
    assert_eq!(count(&db, "SELECT count(*) FROM code_symbols").await, 2);
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM code_symbols WHERE file_id NOT IN (SELECT id FROM code_files)"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM code_files WHERE machine = 'linux'"
        )
        .await,
        1
    );
    assert_eq!(count(&db, "SELECT count(*) FROM code_symbols_fts").await, 2);
    // повтор пути в одной загрузке — последний побеждает
    let dup = vec![
        upload("d.rs", &h('3'), vec![sym("x1", "x1", &bh('1'), 1)], vec![]),
        upload("d.rs", &h('4'), vec![sym("x2", "x2", &bh('2'), 1)], vec![]),
    ];
    code_put_files(&db, G, &meta("box"), dup, &[])
        .await
        .unwrap();
    assert_eq!(
        count(&db, "SELECT count(*) FROM code_files WHERE path = 'd.rs'").await,
        1
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM code_symbols WHERE name IN ('x1', 'x2')"
        )
        .await,
        1
    );
    assert_eq!(
        count(&db, "SELECT count(*) FROM code_symbols WHERE name = 'x2'").await,
        1
    );
}

/// Атрибуция файла и счётчики снимка; преобладающий снимок и доля прочих.
#[tokio::test]
async fn put_files_attribution_and_snapshot_counts() {
    let (db, _) = setup("attrib").await;
    let m = meta("win");
    let user = m.user_id;
    let files = vec![
        upload("a.rs", &h('1'), vec![sym("a", "a", &bh('1'), 1)], vec![]),
        upload(
            "b.rs",
            &h('2'),
            vec![sym("b", "b", &bh('2'), 1), sym("c", "c", &bh('3'), 5)],
            vec![],
        ),
        upload("c.rs", &h('3'), vec![], vec![]),
    ];
    code_put_files(&db, G, &m, files, &[]).await.unwrap();
    let other = UploadMeta {
        commit: Some("def5678".into()),
        branch: Some("dev".into()),
        ..meta("linux")
    };
    code_put_files(
        &db,
        G,
        &other,
        vec![upload("d.rs", &h('4'), vec![], vec![])],
        &[],
    )
    .await
    .unwrap();
    let (commit, branch, machine, uid, sym_n): (String, String, String, String, i64) = db
        .read(|c| {
            Ok(c.query_row(
                "SELECT git_commit, branch, machine, user_id, symbols FROM code_files
                 WHERE path = 'b.rs'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(
        (commit.as_str(), branch.as_str(), machine.as_str()),
        ("abc1234", "main", "win")
    );
    assert_eq!(uid, user.to_string());
    assert_eq!(sym_n, 2);
    let st = code_status(&db, G).await.unwrap().unwrap();
    assert_eq!(st.snapshot.files, 4);
    assert_eq!(st.snapshot.symbols, 3);
    assert_eq!(st.snapshot.commit.as_deref(), Some("abc1234"));
    assert_eq!(st.snapshot.branch.as_deref(), Some("main"));
    assert_eq!(st.snapshot.machine.as_deref(), Some("win"));
    assert_eq!(st.snapshot.rank_state, RankState::Pending);
    assert!((st.other_share - 0.25).abs() < 1e-9, "{}", st.other_share);
}

// ---------- правило устарелости ----------

/// Заметка `a.rs#foo` (через frontmatter) и запись с файловой ссылкой на `a.rs`.
async fn foo_and_file_refs(db: &Db, sol: Id) {
    entry_with(
        db,
        sol,
        "Файловая",
        vec![fref("a.rs", None, Some(h('1')), None)],
    )
    .await;
    entry_with(
        db,
        sol,
        "Символьная",
        vec![fref("a.rs", Some("foo"), Some(h('1')), Some(bh('f')))],
    )
    .await;
}

/// §17 п. 4: правка `bar` — файловая ссылка `stale`, символьная `a.rs#foo` — `ok`.
#[tokio::test]
async fn symbol_ref_ok_when_other_symbol_changed() {
    let (db, sol) = setup("other-changed").await;
    foo_and_file_refs(&db, sol).await;
    let f = changed(
        "a.rs",
        &h('2'),
        Some(vec![("foo", &bh('f')), ("bar", &bh('9'))]),
    );
    let stale = files_changed(&db, G, &[sol], &[f], Timestamp::now())
        .await
        .unwrap();
    assert_eq!(stale, 1);
    let file = ref_row(&db, "a.rs", None).await;
    assert_eq!(file.0, "stale");
    assert_eq!(file.1.as_deref(), Some("source_changed"));
    let s = ref_row(&db, "a.rs", Some("foo")).await;
    assert_eq!((s.0.as_str(), s.1), ("ok", None));
}

/// Правка `foo` — `symbol_changed`; удаление `foo` — `symbol_removed`; возврат тела — `ok`.
#[tokio::test]
async fn symbol_ref_changed_and_removed() {
    let (db, sol) = setup("changed-removed").await;
    foo_and_file_refs(&db, sol).await;
    let now = Timestamp::now();
    let f = changed("a.rs", &h('2'), Some(vec![("foo", &bh('9'))]));
    files_changed(&db, G, &[sol], &[f], now).await.unwrap();
    let s = ref_row(&db, "a.rs", Some("foo")).await;
    assert_eq!(
        (s.0.as_str(), s.1.as_deref()),
        ("stale", Some(SYMBOL_CHANGED))
    );
    // записанный хэш символа не меняется: возврат тела снова даёт `ok`
    assert_eq!(s.3, Some(bh('f')));
    let back = changed("a.rs", &h('3'), Some(vec![("foo", &bh('f'))]));
    files_changed(&db, G, &[sol], &[back], now).await.unwrap();
    assert_eq!(ref_row(&db, "a.rs", Some("foo")).await.0, "ok");
    let gone = changed("a.rs", &h('4'), Some(vec![("bar", &bh('1'))]));
    files_changed(&db, G, &[sol], &[gone], now).await.unwrap();
    let s = ref_row(&db, "a.rs", Some("foo")).await;
    assert_eq!(
        (s.0.as_str(), s.1.as_deref()),
        ("stale", Some(SYMBOL_REMOVED))
    );
}

/// Review Focus 3: удалённый файл (`MISSING_HASH`) — символьные ссылки `symbol_removed`,
/// файловые — `source_changed`.
#[tokio::test]
async fn missing_hash_marks_symbol_refs_removed() {
    let (db, sol) = setup("missing").await;
    foo_and_file_refs(&db, sol).await;
    let f = changed("a.rs", MISSING_HASH, None);
    let stale = files_changed(&db, G, &[sol], &[f], Timestamp::now())
        .await
        .unwrap();
    assert_eq!(stale, 2);
    let s = ref_row(&db, "a.rs", Some("foo")).await;
    assert_eq!(
        (s.0.as_str(), s.1.as_deref()),
        ("stale", Some(SYMBOL_REMOVED))
    );
    let file = ref_row(&db, "a.rs", None).await;
    assert_eq!(file.1.as_deref(), Some("source_changed"));
    // и с присланными символами (пустыми) — то же
    let f = changed("a.rs", MISSING_HASH, Some(vec![("foo", &bh('f'))]));
    files_changed(&db, G, &[sol], &[f], Timestamp::now())
        .await
        .unwrap();
    assert_eq!(
        ref_row(&db, "a.rs", Some("foo")).await.1.as_deref(),
        Some(SYMBOL_REMOVED)
    );
}

/// Символьная ссылка, подтверждённая по символам, принимает новый хэш файла: следующее
/// сообщение без символов с тем же хэшем её не трогает.
#[tokio::test]
async fn symbol_ok_adopts_file_hash() {
    let (db, sol) = setup("adopt-file").await;
    foo_and_file_refs(&db, sol).await;
    let now = Timestamp::now();
    let f = changed("a.rs", &h('2'), Some(vec![("foo", &bh('f'))]));
    files_changed(&db, G, &[sol], &[f], now).await.unwrap();
    let s = ref_row(&db, "a.rs", Some("foo")).await;
    assert_eq!(s.2, Some(h('2')));
    let plain = changed("a.rs", &h('2'), None);
    files_changed(&db, G, &[sol], &[plain], now).await.unwrap();
    assert_eq!(ref_row(&db, "a.rs", Some("foo")).await.0, "ok");
}

/// Без присланных символов — по хэшу файла, как в M4.5.
#[tokio::test]
async fn symbol_ref_without_symbols_falls_back_to_file_hash() {
    let (db, sol) = setup("fallback").await;
    foo_and_file_refs(&db, sol).await;
    let now = Timestamp::now();
    files_changed(&db, G, &[sol], &[changed("a.rs", &h('1'), None)], now)
        .await
        .unwrap();
    assert_eq!(ref_row(&db, "a.rs", Some("foo")).await.0, "ok");
    files_changed(&db, G, &[sol], &[changed("a.rs", &h('2'), None)], now)
        .await
        .unwrap();
    let s = ref_row(&db, "a.rs", Some("foo")).await;
    assert_eq!(
        (s.0.as_str(), s.1.as_deref()),
        ("stale", Some("source_changed"))
    );
}

/// Ссылка без записанного хэша символа (MCP) принимает присланный и остаётся `ok`; символа нет
/// в присланных — `symbol_removed`.
#[tokio::test]
async fn symbol_ref_without_recorded_hash_adopts() {
    let (db, sol) = setup("adopt-symbol").await;
    entry_with(
        &db,
        sol,
        "Без хэшей",
        vec![
            fref("a.rs", Some("foo"), None, None),
            fref("a.rs", Some("ghost"), None, None),
        ],
    )
    .await;
    let f = changed("a.rs", &h('2'), Some(vec![("foo", &bh('f'))]));
    files_changed(&db, G, &[sol], &[f], Timestamp::now())
        .await
        .unwrap();
    let s = ref_row(&db, "a.rs", Some("foo")).await;
    assert_eq!(s.0, "ok");
    assert_eq!(s.3, Some(bh('f')));
    assert_eq!(s.2, Some(h('2')));
    assert_eq!(
        ref_row(&db, "a.rs", Some("ghost")).await.1.as_deref(),
        Some(SYMBOL_REMOVED)
    );
}

/// Review Focus 2: ссылка на `foo#2`; вставка ещё одной `foo` выше — `symbol_changed` (новый
/// `foo#2` — чужое тело); вставка ниже — `ok`.
#[tokio::test]
async fn overload_insert_before_marks_changed_after_keeps_ok() {
    let (db, sol) = setup("overload").await;
    entry_with(
        &db,
        sol,
        "Перегрузка",
        vec![fref("a.rs", Some("foo#2"), Some(h('1')), Some(bh('2')))],
    )
    .await;
    let now = Timestamp::now();
    let after = changed(
        "a.rs",
        &h('2'),
        Some(vec![
            ("foo", &bh('1')),
            ("foo#2", &bh('2')),
            ("foo#3", &bh('3')),
        ]),
    );
    files_changed(&db, G, &[sol], &[after], now).await.unwrap();
    assert_eq!(ref_row(&db, "a.rs", Some("foo#2")).await.0, "ok");
    let before = changed(
        "a.rs",
        &h('3'),
        Some(vec![
            ("foo", &bh('0')),
            ("foo#2", &bh('1')),
            ("foo#3", &bh('2')),
        ]),
    );
    files_changed(&db, G, &[sol], &[before], now).await.unwrap();
    let s = ref_row(&db, "a.rs", Some("foo#2")).await;
    assert_eq!(
        (s.0.as_str(), s.1.as_deref()),
        ("stale", Some(SYMBOL_CHANGED))
    );
}

/// Review Focus 1: загрузка того же файла с другой машины (CRLF — другой хэш файла, те же
/// `body_hash`) — символьные ссылки `ok` и принимают новый хэш.
#[tokio::test]
async fn put_files_crlf_twin_keeps_symbol_refs_ok() {
    let (db, sol) = setup("crlf").await;
    let lf = vec![upload(
        "a.rs",
        &h('1'),
        vec![
            sym("foo", "foo", &bh('f'), 1),
            sym("bar", "bar", &bh('b'), 5),
        ],
        vec![],
    )];
    code_put_files(&db, G, &meta("linux"), lf, &[sol])
        .await
        .unwrap();
    entry_with(
        &db,
        sol,
        "Символы",
        vec![
            fref("a.rs", Some("foo"), Some(h('1')), Some(bh('f'))),
            fref("a.rs", Some("bar"), Some(h('1')), Some(bh('b'))),
        ],
    )
    .await;
    let crlf = vec![upload(
        "a.rs",
        &h('2'),
        vec![
            sym("foo", "foo", &bh('f'), 1),
            sym("bar", "bar", &bh('b'), 5),
        ],
        vec![],
    )];
    let out = code_put_files(&db, G, &meta("win"), crlf, &[sol])
        .await
        .unwrap();
    assert_eq!(out.stale_refs, 0);
    for s in ["foo", "bar"] {
        let r = ref_row(&db, "a.rs", Some(s)).await;
        assert_eq!(r.0, "ok", "{s}");
        assert_eq!(r.2, Some(h('2')), "{s}");
    }
}

/// Неполный разбор (`partial`): найденный символ судится по телу, не найденный — по хэшу файла
/// (символ мог быть обрезан пределом); пропущенный файл — только по хэшу файла.
#[tokio::test]
async fn put_files_partial_and_skipped_do_not_remove_symbols() {
    let (db, sol) = setup("partial").await;
    entry_with(
        &db,
        sol,
        "Символы",
        vec![
            fref("a.rs", Some("foo"), Some(h('1')), Some(bh('f'))),
            fref("a.rs", Some("tail"), Some(h('1')), Some(bh('t'))),
            fref("b.rs", Some("big"), Some(h('1')), Some(bh('g'))),
        ],
    )
    .await;
    let mut a = upload(
        "a.rs",
        &h('2'),
        vec![sym("foo", "foo", &bh('f'), 1)],
        vec![],
    );
    a.parse_state = ParseState::Partial;
    a.skip_reason = Some(SkipReason::Limit);
    let mut b = upload("b.rs", &h('1'), vec![], vec![]);
    b.parse_state = ParseState::Skipped;
    b.skip_reason = Some(SkipReason::TooLarge);
    code_put_files(&db, G, &meta("box"), vec![a, b], &[sol])
        .await
        .unwrap();
    assert_eq!(ref_row(&db, "a.rs", Some("foo")).await.0, "ok");
    let tail = ref_row(&db, "a.rs", Some("tail")).await;
    assert_eq!(tail.1.as_deref(), Some("source_changed"));
    assert_eq!(ref_row(&db, "b.rs", Some("big")).await.0, "ok");
}

/// Правило загрузки касается только ссылок пространств `ref_spaces`.
#[tokio::test]
async fn put_files_applies_rule_only_in_ref_spaces() {
    let (db, sol) = setup("ref-spaces").await;
    let kn = space_of_kind(&db, SpaceKind::Knowledge).await.unwrap().id;
    entry_with(
        &db,
        sol,
        "В решении",
        vec![fref("a.rs", Some("foo"), Some(h('1')), Some(bh('f')))],
    )
    .await;
    entry_with(
        &db,
        kn,
        "В знаниях",
        vec![fref("b.rs", Some("foo"), Some(h('1')), Some(bh('f')))],
    )
    .await;
    let files = vec![
        upload(
            "a.rs",
            &h('2'),
            vec![sym("foo", "foo", &bh('x'), 1)],
            vec![],
        ),
        upload(
            "b.rs",
            &h('2'),
            vec![sym("foo", "foo", &bh('x'), 1)],
            vec![],
        ),
    ];
    let out = code_put_files(&db, G, &meta("box"), files, &[sol])
        .await
        .unwrap();
    assert_eq!(out.stale_refs, 1);
    assert_eq!(ref_row(&db, "a.rs", Some("foo")).await.0, "stale");
    assert_eq!(ref_row(&db, "b.rs", Some("foo")).await.0, "ok");
}

/// `files_verified` с символами: ссылка `ok`, хэш символа — присланный; без символов —
/// хэш символа снимается (ориентиром станет следующее сообщение).
#[tokio::test]
async fn files_verified_takes_symbol_hashes() {
    let (db, sol) = setup("verified").await;
    foo_and_file_refs(&db, sol).await;
    let now = Timestamp::now();
    files_changed(
        &db,
        G,
        &[sol],
        &[changed("a.rs", &h('2'), Some(vec![("foo", &bh('9'))]))],
        now,
    )
    .await
    .unwrap();
    let syms = vec![SymbolHash {
        qual: "foo".into(),
        body_hash: bh('9'),
    }];
    let n = files_verified(
        &db,
        G,
        &[sol],
        &[("a.rs".into(), Some(h('2')), Some(syms))],
        now,
    )
    .await
    .unwrap();
    assert_eq!(n, 2);
    let s = ref_row(&db, "a.rs", Some("foo")).await;
    assert_eq!(
        (s.0.as_str(), s.2, s.3),
        ("ok", Some(h('2')), Some(bh('9')))
    );
    files_verified(&db, G, &[sol], &[("a.rs".into(), None, None)], now)
        .await
        .unwrap();
    let s = ref_row(&db, "a.rs", Some("foo")).await;
    assert_eq!((s.2, s.3), (None, None));
}

// ---------- ссылки: индекс, разбор ----------

/// `refs_index` несёт `symbol`, символьные ссылки — первыми в пути; `refs_to_path` — так же.
#[tokio::test]
async fn refs_index_items_carry_symbol_symbol_first() {
    let (db, sol) = setup("index").await;
    let file = entry_with(&db, sol, "Файловая", vec![fref("a.rs", None, None, None)]).await;
    let symbolic = entry_with(
        &db,
        sol,
        "Символьная",
        vec![fref("a.rs", Some("Store::open"), None, None)],
    )
    .await;
    // файловая — новее, но символьная всё равно первая
    let newer = entry_with(&db, sol, "Новая", vec![fref("a.rs", None, None, None)]).await;
    let idx = refs_index(&db, G, &[sol]).await.unwrap();
    assert_eq!(idx.len(), 1);
    let items = &idx[0].items;
    assert_eq!(items.len(), 3);
    assert_eq!(items[0].symbol.as_deref(), Some("Store::open"));
    assert_eq!(items[0].id, format!("c:{symbolic}"));
    assert_eq!(items[1].symbol, None);
    assert_eq!(items[1].id, format!("c:{newer}"));
    assert_eq!(items[2].id, format!("c:{file}"));
    let json = serde_json::to_value(&idx).unwrap();
    assert!(json[0]["items"][1].get("symbol").is_none(), "{json}");
    let owners = refs_to_path(&db, "a.rs", &[sol]).await.unwrap();
    assert_eq!(owners[0].id, symbolic);
    assert_eq!(owners.len(), 3);
}

/// Строки `путь#qual` frontmatter разбираются `split_ref`; `docs/a#b.md` — путь.
#[tokio::test]
async fn note_refs_split_hash_suffix() {
    let doc = zam_markdown::parse(
        "---\nrefs: [\"a.rs#Store::open\", \"docs/a#b.md\", \"./a.rs\", \"a.rs#Store::open\", \"../x\"]\n---\nтекст\n",
    );
    let refs = note_refs(doc.frontmatter.as_ref());
    assert_eq!(refs.len(), 3, "{refs:?}");
    assert_eq!(refs[0].path, "a.rs");
    assert_eq!(refs[0].symbol.as_deref(), Some("Store::open"));
    assert_eq!(refs[1].path, "docs/a#b.md");
    assert_eq!(refs[1].symbol, None);
    assert_eq!(
        (refs[2].path.as_str(), refs[2].symbol.as_deref()),
        ("a.rs", None)
    );
    // заметка с такими ссылками пишет их в `file_refs`
    let (db, sol) = setup("note-refs").await;
    create_note(
        &db,
        NewNote {
            space_id: sol,
            path: NodePath::parse("notes/store.md").unwrap(),
            content: "---\nrefs: [\"a.rs#Store::open\", a.rs]\n---\nО хранилище.\n".into(),
            l0: None,
            attrib: agent(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        ref_row(&db, "a.rs", Some("Store::open")).await.0,
        "ok".to_string()
    );
    assert_eq!(ref_row(&db, "a.rs", None).await.0, "ok".to_string());
    // changelog: строки `data.refs.files` — так же
    let e = NewEntry {
        refs: None,
        space_id: sol,
        kind: EntryKind::Changelog,
        title: "Правка".into(),
        body: "Тело.".into(),
        data: json!({"kind": "fix", "refs": {"files": ["b.rs#Pool::get", "b.rs"]}}),
        area: None,
        tags: Vec::new(),
        status: None,
        certainty: None,
        idempotency_key: None,
        triggers: Vec::new(),
        links: Vec::new(),
    };
    create_entry(&db, e, &agent()).await.unwrap();
    assert_eq!(ref_row(&db, "b.rs", Some("Pool::get")).await.0, "ok");
    assert_eq!(ref_row(&db, "b.rs", None).await.0, "ok");
}

// ---------- поиск символов, статус, очистка ----------

/// FTS находит `getUserById` по слову `user`.
#[tokio::test]
async fn fts_finds_camel_parts() {
    let (db, _) = setup("fts").await;
    let files = vec![upload(
        "api.ts",
        &h('1'),
        vec![
            sym("getUserById", "UserApi::getUserById", &bh('1'), 3),
            sym("listOrders", "listOrders", &bh('2'), 9),
        ],
        vec![],
    )];
    code_put_files(&db, G, &meta("box"), files, &[])
        .await
        .unwrap();
    let hits = code_symbols_search(&db, G, "user", None, None, 10)
        .await
        .unwrap();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].qual, "UserApi::getUserById");
    assert_eq!(hits[0].path, "api.ts");
    let hits = code_symbols_search(&db, G, "get user", None, None, 10)
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    // синтаксис FTS5 в запросе не действует
    assert!(
        code_symbols_search(&db, G, "\"user\" OR *", None, None, 10)
            .await
            .is_ok()
    );
}

/// Точное совпадение `qual` — первым; `limit` не больше 100; фильтры вида и пути.
#[tokio::test]
async fn symbols_search_exact_qual_first_limit_100() {
    let (db, _) = setup("search").await;
    let mut syms: Vec<Symbol> = (0..150)
        .map(|i| {
            sym(
                &format!("handler{i}"),
                &format!("Handler{i}::handle"),
                &bh('1'),
                i + 1,
            )
        })
        .collect();
    syms.push(Symbol {
        kind: SymbolKind::Struct,
        ..sym("handle", "handle", &bh('2'), 500)
    });
    code_put_files(
        &db,
        G,
        &meta("box"),
        vec![
            upload("src/a.rs", &h('1'), syms, vec![]),
            upload(
                "lib/b.rs",
                &h('2'),
                vec![sym("handle", "b::handle", &bh('3'), 1)],
                vec![],
            ),
        ],
        &[],
    )
    .await
    .unwrap();
    let hits = code_symbols_search(&db, G, "handle", None, None, 500)
        .await
        .unwrap();
    assert_eq!(hits.len(), 100);
    assert_eq!(hits[0].qual, "handle");
    let hits = code_symbols_search(&db, G, "Handler7::handle", None, None, 5)
        .await
        .unwrap();
    assert_eq!(hits[0].qual, "Handler7::handle");
    assert_eq!(hits.len(), 5);
    let structs = code_symbols_search(&db, G, "handle", Some(SymbolKind::Struct), None, 10)
        .await
        .unwrap();
    assert_eq!(structs.len(), 1);
    let lib = code_symbols_search(&db, G, "handle", None, Some("lib"), 10)
        .await
        .unwrap();
    assert_eq!(lib.len(), 1);
    assert_eq!(lib[0].qual, "b::handle");
}

/// Статус: файлы и символы по языкам, пропуски с причинами.
#[tokio::test]
async fn status_by_lang_and_skipped_reasons() {
    let (db, _) = setup("status").await;
    assert!(code_status(&db, G).await.unwrap().is_none());
    let mut big = upload("big.js", &h('3'), vec![], vec![]);
    big.parse_state = ParseState::Skipped;
    big.skip_reason = Some(SkipReason::Minified);
    let mut bin = upload("bin.rs", &h('4'), vec![], vec![]);
    bin.parse_state = ParseState::Skipped;
    bin.skip_reason = Some(SkipReason::Binary);
    let files = vec![
        upload("a.rs", &h('1'), vec![sym("a", "a", &bh('1'), 1)], vec![]),
        upload(
            "b.py",
            &h('2'),
            vec![sym("b", "b", &bh('2'), 1), sym("c", "c", &bh('3'), 3)],
            vec![],
        ),
        big,
        bin,
    ];
    code_put_files(&db, G, &meta("box"), files, &[])
        .await
        .unwrap();
    let st = code_status(&db, G).await.unwrap().unwrap();
    let langs: Vec<(Lang, u64, u64)> = st
        .by_lang
        .iter()
        .map(|l| (l.lang, l.files, l.symbols))
        .collect();
    assert_eq!(
        langs,
        [
            (Lang::Rust, 2, 1),
            (Lang::Python, 1, 2),
            (Lang::JavaScript, 1, 0)
        ]
    );
    assert_eq!(
        st.skipped,
        [
            ("big.js".to_string(), "minified".to_string()),
            ("bin.rs".to_string(), "binary".to_string())
        ]
    );
    assert_eq!(st.skipped_total, 2);
    assert_eq!(st.other_share, 0.0);
}

/// Очистка снимает карту только этого решения.
#[tokio::test]
async fn clear_removes_everything_of_solution_only() {
    let (db, _) = setup("clear").await;
    create_solution(&db, "g-other", "Другое").await.unwrap();
    let f = |p: &str| {
        upload(
            p,
            &h('1'),
            vec![sym("foo", "foo", &bh('1'), 1)],
            vec![rf("x", 1)],
        )
    };
    code_put_files(&db, G, &meta("box"), vec![f("a.rs"), f("b.rs")], &[])
        .await
        .unwrap();
    code_put_files(&db, "g-other", &meta("box"), vec![f("a.rs")], &[])
        .await
        .unwrap();
    assert_eq!(code_clear(&db, G).await.unwrap(), 2);
    assert!(code_status(&db, G).await.unwrap().is_none());
    assert_eq!(count(&db, "SELECT count(*) FROM code_files").await, 1);
    assert_eq!(count(&db, "SELECT count(*) FROM code_symbols").await, 1);
    assert_eq!(count(&db, "SELECT count(*) FROM code_refs").await, 1);
    assert_eq!(count(&db, "SELECT count(*) FROM code_symbols_fts").await, 1);
    assert_eq!(count(&db, "SELECT count(*) FROM code_snapshots").await, 1);
    assert_eq!(code_clear(&db, G).await.unwrap(), 0);
}

/// Невидимость: загрузка карты не трогает чанки, очередь индексации и поиск.
#[tokio::test]
async fn code_tables_not_in_search_or_jobs() {
    let (db, _) = setup("invisible").await;
    let (chunks, jobs) = (
        count(&db, "SELECT count(*) FROM chunks").await,
        count(&db, "SELECT count(*) FROM index_jobs").await,
    );
    let files = vec![upload(
        "a.rs",
        &h('1'),
        vec![sym(
            "квазиуникальныйСимвол",
            "квазиуникальныйСимвол",
            &bh('1'),
            1,
        )],
        vec![],
    )];
    code_put_files(&db, G, &meta("box"), files, &[])
        .await
        .unwrap();
    assert_eq!(count(&db, "SELECT count(*) FROM chunks").await, chunks);
    assert_eq!(count(&db, "SELECT count(*) FROM index_jobs").await, jobs);
    let spaces = list_spaces(&db)
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    let hits = search_fts(
        &db,
        FtsQuery {
            alias_groups: Vec::new(),
            text: "квазиуникальныйСимвол".into(),
            space_ids: spaces,
            path_prefix: None,
            top_k: 10,
            targets: TargetFilter::default(),
        },
    )
    .await
    .unwrap();
    assert!(hits.is_empty(), "{hits:?}");
}

// ---------- ранги, карта, карточка, влияние ----------

/// Три файла: `main.rs` вызывает `parse_config` и `Store`, `cfg.rs` и `store.rs` их определяют.
fn graph_files() -> Vec<FileUpload> {
    vec![
        upload(
            "src/main.rs",
            &h('1'),
            vec![sym("main", "main", &bh('1'), 1)],
            vec![rf("parse_config", 3), rf("Store", 2)],
        ),
        upload(
            "src/cfg.rs",
            &h('2'),
            vec![sym("parse_config", "parse_config", &bh('2'), 1)],
            vec![],
        ),
        upload(
            "src/store.rs",
            &h('3'),
            vec![
                Symbol {
                    kind: SymbolKind::Struct,
                    ..sym("Store", "Store", &bh('3'), 1)
                },
                sym("open", "Store::open", &bh('4'), 5),
            ],
            vec![rf("parse_config", 1)],
        ),
    ]
}

/// Входы ранжирования → `zam_code::rank` → сохранение: ранги файлов и символов,
/// `rank_state = ok`; строки карты упорядочены по рангу, `area` — префикс пути.
#[tokio::test]
async fn rank_inputs_store_ranks_and_map_rows() {
    let (db, _) = setup("rank").await;
    code_put_files(&db, G, &meta("box"), graph_files(), &[])
        .await
        .unwrap();
    let inputs = code_rank_inputs(&db, G).await.unwrap();
    assert_eq!(inputs.files.len(), 3);
    let main = inputs
        .files
        .iter()
        .find(|f| f.path == "src/main.rs")
        .unwrap();
    assert_eq!(
        main.defs,
        [("main".to_string(), SymbolKind::Function, Some(true))]
    );
    assert_eq!(
        main.refs,
        [
            ("Store".to_string(), RefKind::Call, 2),
            ("parse_config".to_string(), RefKind::Call, 3)
        ]
    );
    let ranked = zam_code::rank(&inputs.files, None);
    assert!(code_store_ranks(&db, G, &inputs, &ranked).await.unwrap());
    let st = code_status(&db, G).await.unwrap().unwrap();
    assert_eq!(st.snapshot.rank_state, RankState::Ok);
    assert!(st.snapshot.ranked_at.is_some());
    let rows = code_map_rows(&db, G, None).await.unwrap();
    assert_eq!(rows.len(), 3);
    assert!(rows.windows(2).all(|w| w[0].rank >= w[1].rank));
    // cfg.rs: на него ссылаются оба других — выше main.rs, на который не ссылается никто
    let pos = |p: &str| rows.iter().position(|r| r.path == p).unwrap();
    assert!(pos("src/cfg.rs") < pos("src/main.rs"));
    let store = &rows[pos("src/store.rs")];
    assert_eq!(store.symbols.len(), 2);
    assert!(store.symbols.windows(2).all(|w| w[0].rank >= w[1].rank));
    let share: f64 = store.symbols.iter().map(|s| s.share).sum();
    assert!((share - 1.0).abs() < 1e-9, "{share}");
    assert_eq!(
        code_map_rows(&db, G, Some("src/cfg.rs"))
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(code_map_rows(&db, G, Some("src")).await.unwrap().len(), 3);
    assert!(code_map_rows(&db, G, Some("sr")).await.unwrap().is_empty());
    // новая загрузка — снова `pending`
    code_put_files(&db, G, &meta("box"), vec![graph_files().remove(1)], &[])
        .await
        .unwrap();
    let st = code_status(&db, G).await.unwrap().unwrap();
    assert_eq!(st.snapshot.rank_state, RankState::Pending);
}

/// Карта изменилась между чтением входов и сохранением рангов — ранги не сохраняются,
/// `rank_state` остаётся `pending`.
#[tokio::test]
async fn store_ranks_skipped_when_snapshot_changed() {
    let (db, _) = setup("rank-race").await;
    code_put_files(&db, G, &meta("box"), graph_files(), &[])
        .await
        .unwrap();
    let inputs = code_rank_inputs(&db, G).await.unwrap();
    let ranked = zam_code::rank(&inputs.files, None);
    code_put_files(
        &db,
        G,
        &meta("box"),
        vec![upload("src/new.rs", &h('9'), vec![], vec![])],
        &[],
    )
    .await
    .unwrap();
    assert!(!code_store_ranks(&db, G, &inputs, &ranked).await.unwrap());
    let st = code_status(&db, G).await.unwrap().unwrap();
    assert_eq!(st.snapshot.rank_state, RankState::Pending);
    let rows = code_map_rows(&db, G, None).await.unwrap();
    assert!(
        rows.iter().all(|r| r.rank == 0.0),
        "ранги сохранены по устаревшим входам"
    );
}

/// Карточка символа: определение, кто ссылается на его имя (файлы, число), на что ссылается
/// его файл.
#[tokio::test]
async fn symbol_card_callers_and_uses() {
    let (db, _) = setup("card").await;
    code_put_files(&db, G, &meta("box"), graph_files(), &[])
        .await
        .unwrap();
    let hit = code_symbols_search(&db, G, "parse_config", None, None, 1)
        .await
        .unwrap()
        .remove(0);
    let card = code_symbol(&db, G, hit.id).await.unwrap().unwrap();
    assert_eq!(card.symbol.qual, "parse_config");
    assert_eq!(card.symbol.path, "src/cfg.rs");
    assert_eq!(card.body_hash, bh('2'));
    let callers: Vec<(&str, u32)> = card
        .callers
        .iter()
        .map(|c| (c.path.as_str(), c.count))
        .collect();
    assert_eq!(callers, [("src/main.rs", 3), ("src/store.rs", 1)]);
    let main = code_symbols_search(&db, G, "main", None, None, 1)
        .await
        .unwrap()
        .remove(0);
    let card = code_symbol(&db, G, main.id).await.unwrap().unwrap();
    let uses: Vec<&str> = card.uses.iter().map(|u| u.name.as_str()).collect();
    assert_eq!(uses, ["parse_config", "Store"]);
    // символ другого решения не виден
    assert!(code_symbol(&db, "g-none", hit.id).await.unwrap().is_none());
    assert!(code_symbol(&db, G, 999_999).await.unwrap().is_none());
}

/// Символы для пакета субагенту: точные имена и `qual` — первыми, затем символы путей.
#[tokio::test]
async fn symbols_named_exact_first_then_paths() {
    let (db, _) = setup("named").await;
    code_put_files(&db, G, &meta("box"), graph_files(), &[])
        .await
        .unwrap();
    let hits = code_symbols_named(
        &db,
        G,
        &["Store::open".into(), "parse_config".into()],
        &["src/main.rs".into()],
        5,
    )
    .await
    .unwrap();
    let q: Vec<&str> = hits.iter().map(|h| h.qual.as_str()).collect();
    assert_eq!(q.len(), 3, "{q:?}");
    assert!(q[..2].contains(&"Store::open") && q[..2].contains(&"parse_config"));
    assert_eq!(q[2], "main");
    let one = code_symbols_named(&db, G, &["open".into()], &[], 1)
        .await
        .unwrap();
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].qual, "Store::open");
    assert!(
        code_symbols_named(&db, G, &[], &[], 5)
            .await
            .unwrap()
            .is_empty()
    );
}

/// Влияние: изменённые и удалённые символы против карты и файлы-потребители на 1 шаг.
#[tokio::test]
async fn impact_changed_removed_consumers() {
    let (db, _) = setup("impact").await;
    code_put_files(&db, G, &meta("box"), graph_files(), &[])
        .await
        .unwrap();
    let rows = code_impact(
        &db,
        G,
        &[
            (
                "src/store.rs".into(),
                Some(vec![SymbolHash {
                    qual: "Store".into(),
                    body_hash: bh('x'),
                }]),
            ),
            ("src/cfg.rs".into(), None),
            ("src/unknown.rs".into(), Some(Vec::new())),
        ],
    )
    .await
    .unwrap();
    let changed: Vec<&str> = rows.changed.iter().map(|s| s.qual.as_str()).collect();
    let removed: Vec<&str> = rows.removed.iter().map(|s| s.qual.as_str()).collect();
    assert_eq!(changed, ["Store"]);
    assert_eq!(removed, ["Store::open"]);
    let consumers: Vec<&str> = rows.consumers.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(consumers, ["src/main.rs"]);
    assert_eq!(rows.consumers[0].names, ["Store"]);
    assert_eq!(rows.paths_only, ["src/cfg.rs"]);
}
