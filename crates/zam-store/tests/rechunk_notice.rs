//! Уведомление о перенарезке (§8.2, решение владельца 2026-10-26): рядом с отпечатком нарезки
//! в `meta` хранится JSON её параметров (`chunker_params`, `chunker_entries_params`); при
//! расхождении отпечатка при открытии базы определяется причина и изменившиеся ключи
//! `[index]`, пишется одна строка WARN без текста заметок, ход виден через
//! `UserDbs::rechunk_progress`.
//!
//! Пороги чанкера глобальные на процесс, поэтому тесты не меняют их, а подменяют
//! сохранённые в `meta` отпечаток и параметры — как у базы, нарезанной другими настройками.

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use zam_core::Id;
use zam_store::{
    Attribution, AuthorKind, DataDir, Db, EntryKind, NewEntry, NewNote, NodePath, RechunkReason,
    SpaceKind, UserDbs, create_entry, create_note, list_spaces,
};

async fn meta(db: &Db, key: &'static str) -> Option<String> {
    db.read(move |c| {
        Ok(
            c.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
                .ok(),
        )
    })
    .await
    .unwrap()
}

async fn set_meta(db: &Db, key: &'static str, value: Option<String>) {
    db.write(move |tx| {
        match value {
            Some(v) => tx.execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
                [key, v.as_str()],
            )?,
            None => tx.execute("DELETE FROM meta WHERE key = ?1", [key])?,
        };
        Ok(())
    })
    .await
    .unwrap();
}

/// База с двумя заметками и одной записью журнала (нарезаны текущими настройками).
async fn seeded(name: &str) -> (DataDir, Id, Db) {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    let dd = DataDir::open(&dir).unwrap();
    let user = Id::new();
    let db = UserDbs::new(dd.users_dir()).get(user).await.unwrap();
    let k = list_spaces(&db)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.kind == SpaceKind::Knowledge)
        .unwrap()
        .id;
    for (path, text) in [
        ("первая", "Секретный текст заметки"),
        ("вторая", "Ещё заметка"),
    ] {
        create_note(
            &db,
            NewNote {
                space_id: k,
                path: NodePath::parse(path).unwrap(),
                content: format!("# {path}\n\n{text}\n"),
                l0: None,
                attrib: Attribution::new(AuthorKind::Agent),
            },
        )
        .await
        .unwrap();
    }
    create_entry(
        &db,
        NewEntry {
            space_id: k,
            kind: EntryKind::Insight,
            title: "Запись журнала".into(),
            body: "Тело записи".into(),
            data: serde_json::json!({"kind": "gotcha"}),
            area: None,
            tags: Vec::new(),
            status: None,
            certainty: None,
            idempotency_key: None,
            triggers: Vec::new(),
            links: Vec::new(),
        },
        &Attribution::new(AuthorKind::Agent),
    )
    .await
    .unwrap();
    (dd, user, db)
}

/// Буфер журнала `tracing` для проверки строки WARN.
#[derive(Clone, Default)]
struct Buf(Arc<Mutex<Vec<u8>>>);

impl Write for Buf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Buf {
    type Writer = Buf;
    fn make_writer(&'a self) -> Buf {
        self.clone()
    }
}

#[tokio::test]
async fn rechunk_notice_lists_changed_keys() {
    let (dd, user, db) = seeded("rechunk-notice-keys").await;
    // у базы — текущий отпечаток и рядом JSON параметров с именами ключей `[index]`
    let fp = meta(&db, "chunker").await.expect("отпечаток заметок");
    let params: serde_json::Value = serde_json::from_str(
        &meta(&db, "chunker_params")
            .await
            .expect("параметры заметок"),
    )
    .unwrap();
    assert_eq!(params["chunk_boundary_lambda"], serde_json::json!(0.0));
    assert_eq!(params["algorithm"], serde_json::json!(2));
    assert!(params.get("chunk_prefix_l0").is_some(), "{params}");
    let entry_params: serde_json::Value = serde_json::from_str(
        &meta(&db, "chunker_entries_params")
            .await
            .expect("параметры записей"),
    )
    .unwrap();
    assert!(
        entry_params.get("chunk_prefix_l0").is_none(),
        "у записей флага L0 нет: {entry_params}"
    );

    // заметки нарезаны с λ = 0,5 и другим target_max, записи — только с λ = 0,5
    let mut old = params.clone();
    old["chunk_boundary_lambda"] = serde_json::json!(0.5);
    old["chunk_target_max"] = serde_json::json!(600);
    set_meta(&db, "chunker", Some(fp.replace("lambda=0;", "lambda=0.5;"))).await;
    set_meta(&db, "chunker_params", Some(old.to_string())).await;
    let efp = meta(&db, "chunker_entries").await.unwrap();
    let mut eold = entry_params.clone();
    eold["chunk_boundary_lambda"] = serde_json::json!(0.5);
    set_meta(
        &db,
        "chunker_entries",
        Some(efp.replace("lambda=0;", "lambda=0.5;")),
    )
    .await;
    set_meta(&db, "chunker_entries_params", Some(eold.to_string())).await;

    let buf = Buf::default();
    let sub = tracing_subscriber::fmt()
        .with_writer(buf.clone())
        .with_ansi(false)
        .finish();
    let dbs = UserDbs::new(dd.users_dir());
    let reopened = {
        let _g = tracing::subscriber::set_default(sub);
        dbs.get(user).await.unwrap()
    };
    let notice = dbs.opened_rechunk(user).await.expect("уведомление").notice;
    assert_eq!(notice.reason, RechunkReason::Params);
    assert_eq!(
        notice.changed,
        vec!["chunk_target_max", "chunk_boundary_lambda"],
        "ключи обеих частей без повторов, в порядке [index]"
    );
    assert_eq!(notice.targets, 3, "две заметки и запись");
    let log = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    let warn: Vec<&str> = log.lines().filter(|l| l.contains("WARN")).collect();
    assert_eq!(warn.len(), 1, "{log}");
    assert!(
        warn[0].contains(&format!(
            "перенарезка базы {user}: изменились chunk_target_max, chunk_boundary_lambda — 3 записей"
        )),
        "{log}"
    );
    assert!(
        !log.contains("Секретный текст"),
        "текст заметок в журнал не попадает"
    );

    dbs.wait_rechunk(user).await;
    assert_eq!(
        dbs.rechunk_progress(user).await,
        None,
        "перенарезка закончилась"
    );
    // `done` считает те же цели, что `total`: все живые заметки и записи
    let fin = dbs.opened_rechunk(user).await.unwrap();
    assert_eq!(fin.done, fin.notice.targets);
    // после перенарезки — текущие отпечаток и параметры
    assert_eq!(meta(&reopened, "chunker").await.unwrap(), fp);
    assert_eq!(
        meta(&reopened, "chunker_params").await.unwrap(),
        params.to_string()
    );
    // следующее открытие — без перенарезки
    let again = UserDbs::new(dd.users_dir());
    again.get(user).await.unwrap();
    assert_eq!(again.opened_rechunk(user).await, None);
}

#[tokio::test]
async fn rechunk_notice_for_m3_db_unknown() {
    let (dd, user, db) = seeded("rechunk-notice-m3").await;
    let fp = meta(&db, "chunker").await.unwrap();
    let efp = meta(&db, "chunker_entries").await.unwrap();
    // база M3: отпечатки с λ = 0,5 (умолчание M3), JSON параметров нет
    set_meta(&db, "chunker", Some(fp.replace("lambda=0;", "lambda=0.5;"))).await;
    set_meta(
        &db,
        "chunker_entries",
        Some(efp.replace("lambda=0;", "lambda=0.5;")),
    )
    .await;
    set_meta(&db, "chunker_params", None).await;
    set_meta(&db, "chunker_entries_params", None).await;
    let dbs = UserDbs::new(dd.users_dir());
    dbs.get(user).await.unwrap();
    let notice = dbs.opened_rechunk(user).await.expect("уведомление").notice;
    assert_eq!(notice.reason, RechunkReason::Unknown);
    // ключи восстановлены из отпечатка M3
    assert_eq!(notice.changed, vec!["chunk_boundary_lambda"]);
    assert_eq!(notice.targets, 3);
    dbs.wait_rechunk(user).await;
    assert_eq!(dbs.opened_rechunk(user).await.unwrap().done, 3);
    let db = dbs.get(user).await.unwrap();
    assert!(
        meta(&db, "chunker_params").await.is_some(),
        "после перенарезки JSON параметров сохранён"
    );

    // отпечаток до задачи 17 M3 (не разбирается) и без JSON — причина неизвестна, ключей нет
    set_meta(&db, "chunker", None).await;
    set_meta(&db, "chunker_params", None).await;
    let dbs = UserDbs::new(dd.users_dir());
    dbs.get(user).await.unwrap();
    let notice = dbs.opened_rechunk(user).await.expect("уведомление").notice;
    assert_eq!(notice.reason, RechunkReason::Unknown);
    assert!(notice.changed.is_empty(), "{:?}", notice.changed);
    assert_eq!(notice.targets, 2, "перенарезаются только заметки");
    dbs.wait_rechunk(user).await;
    assert_eq!(dbs.opened_rechunk(user).await.unwrap().done, 2);
}
