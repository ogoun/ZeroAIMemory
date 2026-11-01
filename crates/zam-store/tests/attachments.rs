//! Вложения как цель индексации (задача 4 M5): `attachment_docs`, кэш `blob_extract`, чанки
//! вложений (`chunks.attachment_id`), изоляция от чанков заметки и каскад операций над узлом
//! (Review Focus 2 и 3 плана M5), миграция 0019 с базы M4.5.

use std::path::PathBuf;

use zam_core::{Id, TargetKind};
use zam_store::attachments::{
    AttachmentDoc, DocIntake, DocKind, ExtractState, Extracted, Locator, Unit, detach,
    extract_cached, list_attachments, put_extract, replace_attachment_chunks, set_extract_state,
};
use zam_store::blobs::{AttachScope, BlobStore, attach, resolve_attachment};
use zam_store::{
    Attribution, AuthorKind, DataDir, Db, FtsQuery, NewNote, NodePath, SpaceKind, USER_MIGRATIONS,
    UserDbs, claim_jobs, create_note, delete_node, list_spaces, move_node, restore_node,
    search_fts, update_note,
};

const TS: &str = "2026-10-28T00:00:00.000000Z";

fn datadir(name: &str) -> DataDir {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("attachments-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    DataDir::open(&dir).unwrap()
}

fn human() -> Attribution {
    Attribution::new(AuthorKind::Human)
}

async fn note(db: &Db, space: Id, path: &str, content: &str) -> Id {
    create_note(
        db,
        NewNote {
            space_id: space,
            path: NodePath::parse(path).unwrap(),
            content: content.into(),
            l0: None,
            attrib: human(),
        },
    )
    .await
    .unwrap()
    .id
}

/// База пользователя, хранилище блобов, пространства Knowledge и Human (id, имя).
struct Env {
    _dd: DataDir,
    db: Db,
    bs: BlobStore,
    k: Id,
    k_name: String,
    h: Id,
}

async fn setup(name: &str) -> Env {
    let dd = datadir(name);
    let user = Id::new();
    let db = UserDbs::new(dd.users_dir()).get(user).await.unwrap();
    let spaces = list_spaces(&db).await.unwrap();
    let find = |k: SpaceKind| spaces.iter().find(|s| s.kind == k).unwrap().clone();
    let (k, h) = (find(SpaceKind::Knowledge), find(SpaceKind::Human));
    let bs = BlobStore::new(&dd.blobs_dir(), user);
    Env {
        _dd: dd,
        db,
        bs,
        k: k.id,
        k_name: k.name,
        h: h.id,
    }
}

/// Кладёт `data` в блобы и привязывает к `node` как `name` вида `kind` (без предела размера).
async fn put_attach(env: &Env, node: Id, data: &[u8], name: &str, kind: Option<DocKind>) -> String {
    put_attach_max(env, node, data, name, kind, u64::MAX).await
}

/// [`put_attach`] с пределом `docs.max_bytes` = `max`.
async fn put_attach_max(
    env: &Env,
    node: Id,
    data: &[u8],
    name: &str,
    kind: Option<DocKind>,
    max: u64,
) -> String {
    let (sha, size) = env.bs.put(data, 1 << 20).await.unwrap();
    attach(
        &env.db,
        node,
        &sha,
        size,
        name,
        "text/plain",
        DocIntake::classify(kind, size, max),
        AuthorKind::Human,
    )
    .await
    .unwrap();
    sha
}

fn doc_of(list: &[AttachmentDoc], name: &str) -> AttachmentDoc {
    list.iter()
        .find(|d| d.filename == name)
        .unwrap_or_else(|| panic!("нет вложения {name}: {list:?}"))
        .clone()
}

fn units(texts: &[(&str, &str)]) -> Vec<Unit> {
    texts
        .iter()
        .enumerate()
        .map(|(i, (h, t))| Unit {
            locator: Locator::Para(i as u32 + 1),
            heading_path: if h.is_empty() {
                Vec::new()
            } else {
                vec![h.to_string()]
            },
            text: t.to_string(),
        })
        .collect()
}

/// Строки чанков: `(seq, id, attachment_id, context_prefix, text, level, locator, blob_sha256)`.
type ChunkDump = (
    i64,
    String,
    Option<String>,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
);

async fn chunks_of(db: &Db, node: Id) -> Vec<ChunkDump> {
    db.read(move |c| {
        let mut st = c.prepare(
            "SELECT seq, id, attachment_id, context_prefix, text, level, locator, blob_sha256
             FROM chunks WHERE node_id = ?1 ORDER BY attachment_id IS NOT NULL, ordinal",
        )?;
        let rows = st.query_map([node], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
            ))
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    })
    .await
    .unwrap()
}

fn of_attachment(rows: &[ChunkDump], att: Id) -> Vec<ChunkDump> {
    let a = att.to_string();
    rows.iter()
        .filter(|r| r.2.as_deref() == Some(a.as_str()))
        .cloned()
        .collect()
}

fn of_note(rows: &[ChunkDump]) -> Vec<ChunkDump> {
    rows.iter().filter(|r| r.2.is_none()).cloned().collect()
}

/// Активное задание цели: `(target_kind, op)`.
async fn active_job(db: &Db, kind: &str, id: Id) -> Option<String> {
    let kind = kind.to_string();
    db.read(move |c| {
        use rusqlite::OptionalExtension;
        Ok(c.query_row(
            "SELECT op FROM index_jobs
             WHERE target_kind = ?1 AND target_id = ?2 AND state IN ('queued', 'running')",
            rusqlite::params![kind, id],
            |r| r.get(0),
        )
        .optional()?)
    })
    .await
    .unwrap()
}

/// Снимает задания цели (имитация выполненного воркером задания).
async fn drop_jobs(db: &Db) {
    db.write(|t| {
        t.execute("DELETE FROM index_jobs", [])?;
        Ok(())
    })
    .await
    .unwrap();
}

fn fts(text: &str, spaces: &[Id]) -> FtsQuery {
    FtsQuery {
        alias_groups: Vec::new(),
        text: text.into(),
        space_ids: spaces.to_vec(),
        path_prefix: None,
        top_k: 20,
        targets: Default::default(),
    }
}

/// Миграция 0019 на базе M4.5 (v18): `attachment_docs` дозаполняется из `attachments`; каждое
/// вложение живого узла — `pending` с заданием `attachment` (вид по содержимому определит
/// конвейер задачи 5: `.xlsm`, PDF без расширения и т. п. — тоже документы; раунд правок 1),
/// вложение удалённого узла — `pending`, скрыто той же меткой и задания не получает.
#[tokio::test]
async fn migration_0019_backfills_attachment_docs() {
    let dd = datadir("mig-0019");
    let user = Id::new();
    let path = dd.user_db(&user);
    let (space, live, gone) = (Id::new(), Id::new(), Id::new());
    {
        // схема v18 — строками SQL: код записи узлов уже знает `chunks.attachment_id`
        let mut c = rusqlite::Connection::open(&path).unwrap();
        let v = zam_store::migrate::apply(&mut c, &path, &USER_MIGRATIONS[..18]).unwrap();
        assert_eq!(v, 18);
        c.execute(
            "INSERT INTO spaces (id, kind, name, created_at) VALUES (?1, 'knowledge', 'K', ?2)",
            rusqlite::params![space, TS],
        )
        .unwrap();
        let ins_node = |id: Id, path: &str, del: Option<&str>| {
            c.execute(
                "INSERT INTO nodes (id, space_id, parent_id, kind, name, path, index_state,
                                    created_at, updated_at, deleted_at)
                 VALUES (?1, ?2, NULL, 'note', ?3, ?3, 'indexed', ?5, ?5, ?4)",
                rusqlite::params![id, space, path, del, TS],
            )
            .unwrap();
        };
        ins_node(live, "live", None);
        ins_node(gone, "gone", Some(TS));
        let (a, b, d) = ("a".repeat(64), "b".repeat(64), "c".repeat(64));
        let (x, y) = ("d".repeat(64), "e".repeat(64));
        for sha in [&a, &b, &d, &x, &y] {
            c.execute(
                "INSERT INTO blobs (sha256, size, mime, refcount, created_at)
                 VALUES (?1, 3, NULL, 1, ?2)",
                rusqlite::params![sha, TS],
            )
            .unwrap();
        }
        let ins = |node: Id, sha: &str, name: &str| {
            c.execute(
                "INSERT INTO attachments (node_id, blob_sha256, filename, mime, size, created_at)
                 VALUES (?1, ?2, ?3, NULL, 3, ?4)",
                rusqlite::params![node, sha, name, TS],
            )
            .unwrap();
        };
        ins(live, &a, "Отчёт.PDF");
        ins(live, &b, "cat.png");
        ins(live, &x, "book.xlsm");
        ins(live, &y, "scan");
        ins(gone, &d, "plan.docx");
    }
    let db = UserDbs::new(dd.users_dir()).get(user).await.unwrap();
    let docs = list_attachments(&db, live).await.unwrap();
    assert_eq!(docs.len(), 4, "{docs:?}");
    let pdf = doc_of(&docs, "Отчёт.PDF");
    let png = doc_of(&docs, "cat.png");
    assert_eq!(pdf.blob_sha256, "a".repeat(64));
    assert_ne!(pdf.id, png.id);
    for name in ["Отчёт.PDF", "cat.png", "book.xlsm", "scan"] {
        let d = doc_of(&docs, name);
        assert_eq!(d.extract_state, ExtractState::Pending, "{name}");
        assert_eq!(
            active_job(&db, "attachment", d.id).await.as_deref(),
            Some("upsert"),
            "{name}"
        );
    }
    // вложение удалённого узла: строка есть, скрыта меткой узла, без задания
    assert!(list_attachments(&db, gone).await.unwrap().is_empty());
    let (hidden, same_stamp, jobs): (i64, bool, i64) = db
        .read(move |c| {
            Ok(c.query_row(
                "SELECT count(*), coalesce(min(d.deleted_at = n.deleted_at), 0),
                        (SELECT count(*) FROM index_jobs j WHERE j.target_id = d.id)
                 FROM attachment_docs d JOIN nodes n ON n.id = d.node_id
                 WHERE d.node_id = ?1",
                [gone],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?)
        })
        .await
        .unwrap();
    assert_eq!((hidden, same_stamp, jobs), (1, true, 0));
    let gone_state: String = db
        .read(move |c| {
            Ok(c.query_row(
                "SELECT extract_state FROM attachment_docs WHERE node_id = ?1",
                [gone],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(gone_state, "pending");
    // id дозаполнения — обычные Id (26 символов base32)
    assert_eq!(pdf.id.to_string().len(), 26);
}

/// `attach` поддерживаемого вида ставит задание `Attachment` (`pending`); неподдерживаемого —
/// `unsupported` без задания; повтор пары задание не дублирует.
#[tokio::test]
async fn attach_enqueues_attachment_job() {
    let env = setup("enqueue").await;
    let n = note(&env.db, env.k, "n", "текст").await;
    drop_jobs(&env.db).await;
    put_attach(&env, n, b"%PDF-1.4 x", "a.pdf", Some(DocKind::Pdf)).await;
    put_attach(&env, n, b"\x89PNG", "cat.png", None).await;
    put_attach(&env, n, b"%PDF-1.4 x", "a.pdf", Some(DocKind::Pdf)).await;
    let docs = list_attachments(&env.db, n).await.unwrap();
    assert_eq!(docs.len(), 2);
    let pdf = doc_of(&docs, "a.pdf");
    let png = doc_of(&docs, "cat.png");
    assert_eq!(pdf.extract_state, ExtractState::Pending);
    assert_eq!(png.extract_state, ExtractState::Unsupported);
    assert_eq!(pdf.node_id, n);
    let jobs = claim_jobs(&env.db, 10, jiff::Timestamp::now())
        .await
        .unwrap();
    assert_eq!(jobs.len(), 1, "{jobs:?}");
    assert_eq!(jobs[0].target_kind, TargetKind::Attachment);
    assert_eq!(jobs[0].target_id, pdf.id);
    assert_eq!(jobs[0].target().to_string(), format!("a:{}", pdf.id));

    // Human: извлечение нужно (FTS), задание ставится; точек нет — чанки задания не ставят
    let hn = note(&env.db, env.h, "личное", "текст").await;
    drop_jobs(&env.db).await;
    put_attach(&env, hn, b"hello human", "h.txt", Some(DocKind::Text)).await;
    let hd = doc_of(&list_attachments(&env.db, hn).await.unwrap(), "h.txt");
    assert_eq!(
        active_job(&env.db, "attachment", hd.id).await.as_deref(),
        Some("upsert")
    );
    drop_jobs(&env.db).await;
    replace_attachment_chunks(&env.db, hd.id, &units(&[("", "человеческий текст")]))
        .await
        .unwrap();
    assert_eq!(active_job(&env.db, "attachment", hd.id).await, None);
    // а у Knowledge — ставят (точки Qdrant)
    replace_attachment_chunks(&env.db, pdf.id, &units(&[("", "текст pdf")]))
        .await
        .unwrap();
    assert_eq!(
        active_job(&env.db, "attachment", pdf.id).await.as_deref(),
        Some("upsert")
    );
    // состояние извлечения
    set_extract_state(&env.db, pdf.id, ExtractState::Failed, Some("parse_crash"))
        .await
        .unwrap();
    let pdf = doc_of(&list_attachments(&env.db, n).await.unwrap(), "a.pdf");
    assert_eq!(pdf.extract_state, ExtractState::Failed);
    assert_eq!(pdf.extract_error.as_deref(), Some("parse_crash"));
}

/// Review Focus 2: правка текста заметки не трогает чанки её вложений (те же строки и `seq`,
/// FTS не переписан) и не ставит заданий вложениям; поиск по заметкам чанков вложений не
/// находит (поиск по вложениям — задача 6).
#[tokio::test]
async fn note_edit_keeps_attachment_chunks() {
    let env = setup("edit").await;
    let n = note(&env.db, env.k, "n", "# Заметка\n\nпервая версия текста").await;
    put_attach(&env, n, b"doc", "doc.txt", Some(DocKind::Text)).await;
    let d = doc_of(&list_attachments(&env.db, n).await.unwrap(), "doc.txt");
    replace_attachment_chunks(
        &env.db,
        d.id,
        &units(&[("Глава", "уникальноеслововложения раз"), ("", "два")]),
    )
    .await
    .unwrap();
    let before = chunks_of(&env.db, n).await;
    let att_before = of_attachment(&before, d.id);
    assert_eq!(att_before.len(), 2);
    assert!(att_before.iter().all(|r| r.5 == "attachment"));
    assert_eq!(att_before[0].6.as_deref(), Some("para:1"));
    assert_eq!(att_before[0].7.as_deref(), Some(d.blob_sha256.as_str()));
    drop_jobs(&env.db).await;

    let node = zam_store::get_node(&env.db, n).await.unwrap();
    update_note(
        &env.db,
        n,
        node.current_rev,
        "# Заметка\n\nвторая версия, совсем другой текст".into(),
        None,
        human(),
    )
    .await
    .unwrap();
    let after = chunks_of(&env.db, n).await;
    assert_eq!(
        of_attachment(&after, d.id),
        att_before,
        "чанки вложения изменились"
    );
    assert_ne!(
        of_note(&after),
        of_note(&before),
        "чанки заметки не пересобраны"
    );
    assert_eq!(active_job(&env.db, "attachment", d.id).await, None);
    assert_eq!(
        active_job(&env.db, "node", n).await.as_deref(),
        Some("upsert")
    );
    // повторная запись вложения тем же содержимым — без перезаписи (FTS не трогается)
    replace_attachment_chunks(
        &env.db,
        d.id,
        &units(&[("Глава", "уникальноеслововложения раз"), ("", "два")]),
    )
    .await
    .unwrap();
    assert_eq!(
        of_attachment(&chunks_of(&env.db, n).await, d.id),
        att_before
    );
    // изоляция в поиске: слово только во вложении — пусто; слово заметки находится
    assert!(
        search_fts(&env.db, fts("уникальноеслововложения", &[env.k]))
            .await
            .unwrap()
            .is_empty()
    );
    let hits = search_fts(&env.db, fts("вторая", &[env.k])).await.unwrap();
    assert!(!hits.is_empty());
    assert!(hits.iter().all(|h| h.level != "attachment"));
}

/// Перенос узла пересчитывает `context_prefix` чанков вложений по их формуле
/// (`[<space>/<путь заметки>] <имя файла> > <heading_path>`) и ставит им `upsert`.
#[tokio::test]
async fn node_move_recomputes_attachment_prefix() {
    let env = setup("move").await;
    let n = note(&env.db, env.k, "old/n", "текст").await;
    put_attach(&env, n, b"doc", "Отчёт.txt", Some(DocKind::Text)).await;
    let d = doc_of(&list_attachments(&env.db, n).await.unwrap(), "Отчёт.txt");
    replace_attachment_chunks(&env.db, d.id, &units(&[("Глава 1", "раз"), ("", "два")]))
        .await
        .unwrap();
    let rows = of_attachment(&chunks_of(&env.db, n).await, d.id);
    let k = &env.k_name;
    assert_eq!(rows[0].3, format!("[{k}/old/n] Отчёт.txt > Глава 1"));
    assert_eq!(rows[1].3, format!("[{k}/old/n] Отчёт.txt"));
    drop_jobs(&env.db).await;

    // перенос родительской папки: вложение потомка тоже
    let folder = zam_store::get_node_by_path(&env.db, env.k, &NodePath::parse("old").unwrap())
        .await
        .unwrap();
    move_node(
        &env.db,
        folder.id,
        NodePath::parse("new/place").unwrap(),
        AuthorKind::Human,
    )
    .await
    .unwrap();
    let all = chunks_of(&env.db, n).await;
    let rows = of_attachment(&all, d.id);
    assert_eq!(rows[0].3, format!("[{k}/new/place/n] Отчёт.txt > Глава 1"));
    assert_eq!(rows[1].3, format!("[{k}/new/place/n] Отчёт.txt"));
    // префиксы заметки — по формуле заметки (имя узла, не файла)
    assert!(
        of_note(&all)
            .iter()
            .all(|r| r.3.starts_with(&format!("[{k}/new/place/n] n")))
    );
    assert_eq!(
        active_job(&env.db, "attachment", d.id).await.as_deref(),
        Some("upsert")
    );
}

/// Удаление узла скрывает его вложения (список, задание `delete`), чанки не теряются;
/// восстановление возвращает вложения и ставит `upsert`.
#[tokio::test]
async fn node_delete_hides_attachments_restore_returns() {
    let env = setup("delete").await;
    let n = note(&env.db, env.k, "n", "текст").await;
    put_attach(&env, n, b"doc", "doc.txt", Some(DocKind::Text)).await;
    put_attach(&env, n, b"\x89PNG", "cat.png", None).await;
    let d = doc_of(&list_attachments(&env.db, n).await.unwrap(), "doc.txt");
    set_extract_state(&env.db, d.id, ExtractState::Done, None)
        .await
        .unwrap();
    replace_attachment_chunks(&env.db, d.id, &units(&[("", "раз"), ("", "два")]))
        .await
        .unwrap();
    let att_before = of_attachment(&chunks_of(&env.db, n).await, d.id);
    drop_jobs(&env.db).await;

    delete_node(&env.db, n, AuthorKind::Human).await.unwrap();
    assert!(list_attachments(&env.db, n).await.unwrap().is_empty());
    assert_eq!(
        active_job(&env.db, "attachment", d.id).await.as_deref(),
        Some("delete")
    );
    // чанки заметки удалены, чанки вложения остаются (скрыты меткой удаления)
    let mid = chunks_of(&env.db, n).await;
    assert!(of_note(&mid).is_empty());
    assert_eq!(of_attachment(&mid, d.id), att_before);
    drop_jobs(&env.db).await;

    restore_node(&env.db, n, AuthorKind::Human).await.unwrap();
    let docs = list_attachments(&env.db, n).await.unwrap();
    assert_eq!(docs.len(), 2);
    let back = doc_of(&docs, "doc.txt");
    assert_eq!((back.id, back.extract_state), (d.id, ExtractState::Done));
    assert_eq!(
        of_attachment(&chunks_of(&env.db, n).await, d.id),
        att_before
    );
    assert_eq!(
        active_job(&env.db, "attachment", d.id).await.as_deref(),
        Some("upsert")
    );
    let png = doc_of(&docs, "cat.png");
    assert_eq!(active_job(&env.db, "attachment", png.id).await, None);
}

/// Открепление: строки `attachments` и чанков вложения нет, `refcount − 1`, задание `delete`,
/// ссылка не разрешается; блоб остаётся (GC — M7). Повторная привязка возвращает тот же id.
#[tokio::test]
async fn detach_removes_chunks() {
    let env = setup("detach").await;
    let n = note(&env.db, env.k, "n", "текст").await;
    let sha = put_attach(&env, n, b"doc", "doc.txt", Some(DocKind::Text)).await;
    let d = doc_of(&list_attachments(&env.db, n).await.unwrap(), "doc.txt");
    replace_attachment_chunks(&env.db, d.id, &units(&[("", "раз")]))
        .await
        .unwrap();
    drop_jobs(&env.db).await;

    detach(&env.db, n, &sha, "doc.txt", &human()).await.unwrap();
    assert!(list_attachments(&env.db, n).await.unwrap().is_empty());
    assert!(of_attachment(&chunks_of(&env.db, n).await, d.id).is_empty());
    assert_eq!(
        active_job(&env.db, "attachment", d.id).await.as_deref(),
        Some("delete")
    );
    let sha2 = sha.clone();
    let refcount: i64 = env
        .db
        .read(move |c| {
            Ok(c.query_row(
                "SELECT refcount FROM blobs WHERE sha256 = ?1",
                [sha2],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(refcount, 0);
    assert!(env.bs.path_of(&sha).exists(), "блоб удалён до GC");
    let scope = AttachScope {
        prefer_space: None,
        allow_human: true,
        only_space: false,
    };
    assert_eq!(
        resolve_attachment(&env.db, &sha[..16], "doc.txt", scope)
            .await
            .unwrap(),
        None
    );
    // повторное открепление — NotFound
    assert!(matches!(
        detach(&env.db, n, &sha, "doc.txt", &human()).await,
        Err(zam_store::StoreError::NotFound)
    ));
    // повторная привязка — тот же документ, снова `pending`
    put_attach(&env, n, b"doc", "doc.txt", Some(DocKind::Text)).await;
    let again = doc_of(&list_attachments(&env.db, n).await.unwrap(), "doc.txt");
    assert_eq!(again.id, d.id);
    assert_eq!(again.extract_state, ExtractState::Pending);
}

/// Review Focus 3: один блоб в двух заметках — два документа (свои id, чанки и задания), а
/// извлечение — одно на `blob_sha256` (кэш); база другого пользователя кэша не видит.
#[tokio::test]
async fn same_blob_two_notes_two_docs() {
    let env = setup("shared").await;
    let a = note(&env.db, env.k, "a", "текст").await;
    let b = note(&env.db, env.h, "b", "текст").await;
    let sha = put_attach(&env, a, b"same", "x.txt", Some(DocKind::Text)).await;
    put_attach(&env, b, b"same", "x.txt", Some(DocKind::Text)).await;
    let da = doc_of(&list_attachments(&env.db, a).await.unwrap(), "x.txt");
    let db_ = doc_of(&list_attachments(&env.db, b).await.unwrap(), "x.txt");
    assert_ne!(da.id, db_.id);
    assert_eq!(da.blob_sha256, db_.blob_sha256);

    assert_eq!(extract_cached(&env.db, &sha).await.unwrap(), None);
    let ex = Extracted {
        kind: DocKind::Xlsx,
        units: vec![
            Unit {
                locator: Locator::Sheet {
                    name: "'Итоги' 2026!".into(),
                    range: "A1:F40".into(),
                },
                heading_path: vec!["Итоги".into()],
                text: "| a | b |".into(),
            },
            Unit {
                locator: Locator::Page(12),
                heading_path: Vec::new(),
                text: "страница".into(),
            },
            Unit {
                locator: Locator::Slide(3),
                heading_path: vec!["A".into(), "B".into()],
                text: "слайд".into(),
            },
        ],
        warnings: vec!["truncated: max_cells".into()],
        ocr_needed: true,
    };
    put_extract(&env.db, &sha, &ex).await.unwrap();
    // повтор — без ошибки (тот же ключ кэша)
    put_extract(&env.db, &sha, &ex).await.unwrap();
    assert_eq!(
        extract_cached(&env.db, &sha).await.unwrap(),
        Some(ex.clone())
    );
    let n_cache: i64 = env
        .db
        .read(|c| Ok(c.query_row("SELECT count(*) FROM blob_extract", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(n_cache, 1);

    replace_attachment_chunks(&env.db, da.id, &ex.units)
        .await
        .unwrap();
    replace_attachment_chunks(&env.db, db_.id, &ex.units)
        .await
        .unwrap();
    let ca = of_attachment(&chunks_of(&env.db, a).await, da.id);
    let cb = of_attachment(&chunks_of(&env.db, b).await, db_.id);
    assert_eq!((ca.len(), cb.len()), (3, 3));
    assert!(ca.iter().all(|x| cb.iter().all(|y| x.1 != y.1)));
    // `done` берёт `ocr_needed` из кэша извлечения
    set_extract_state(&env.db, da.id, ExtractState::Done, None)
        .await
        .unwrap();
    let da = doc_of(&list_attachments(&env.db, a).await.unwrap(), "x.txt");
    assert!(da.ocr_needed);
    let db_ = doc_of(&list_attachments(&env.db, b).await.unwrap(), "x.txt");
    assert!(!db_.ocr_needed);

    // другой пользователь: своя база, тот же sha — кэша нет, вложений нет
    let other = setup("shared-other").await;
    assert_eq!(extract_cached(&other.db, &sha).await.unwrap(), None);
    let on = note(&other.db, other.k, "a", "текст").await;
    assert!(list_attachments(&other.db, on).await.unwrap().is_empty());
}

/// id чанков вложения детерминированы, различны внутри документа (даже при одинаковом
/// тексте), между документами и с чанками заметки.
#[tokio::test]
async fn attachment_chunk_ids_unique() {
    let env = setup("ids").await;
    let n = note(&env.db, env.k, "n", "раз").await;
    put_attach(&env, n, b"one", "1.txt", Some(DocKind::Text)).await;
    put_attach(&env, n, b"two", "2.txt", Some(DocKind::Text)).await;
    let docs = list_attachments(&env.db, n).await.unwrap();
    let (d1, d2) = (doc_of(&docs, "1.txt"), doc_of(&docs, "2.txt"));
    let same = units(&[("", "раз"), ("", "раз"), ("", "раз")]);
    replace_attachment_chunks(&env.db, d1.id, &same)
        .await
        .unwrap();
    replace_attachment_chunks(&env.db, d2.id, &same)
        .await
        .unwrap();
    let all = chunks_of(&env.db, n).await;
    let ids: Vec<&String> = all.iter().map(|r| &r.1).collect();
    let uniq: std::collections::HashSet<&&String> = ids.iter().collect();
    assert_eq!(ids.len(), uniq.len(), "{ids:?}");
    assert_eq!(of_attachment(&all, d1.id).len(), 3);
    assert!(
        ids.iter()
            .all(|i| i.len() == 32 && i.bytes().all(|b| b.is_ascii_hexdigit()))
    );
    // детерминизм: другая нарезка и обратно — те же id
    let first: Vec<String> = of_attachment(&all, d1.id)
        .into_iter()
        .map(|r| r.1)
        .collect();
    replace_attachment_chunks(&env.db, d1.id, &units(&[("", "иначе")]))
        .await
        .unwrap();
    replace_attachment_chunks(&env.db, d1.id, &same)
        .await
        .unwrap();
    let again: Vec<String> = of_attachment(&chunks_of(&env.db, n).await, d1.id)
        .into_iter()
        .map(|r| r.1)
        .collect();
    assert_eq!(first, again);
}

/// Поддерживаемый файл больше `docs.max_bytes` — `failed` с `too_large` без задания (раунд правок
/// 1); повторная привязка после открепления переносит документ в конец списка (`created_at`).
#[tokio::test]
async fn too_large_failed_and_reattach_moves_to_end() {
    let env = setup("too-large").await;
    let n = note(&env.db, env.k, "n", "текст").await;
    drop_jobs(&env.db).await;
    put_attach_max(&env, n, b"%PDF-1.4 big", "big.pdf", Some(DocKind::Pdf), 4).await;
    let big = doc_of(&list_attachments(&env.db, n).await.unwrap(), "big.pdf");
    assert_eq!(big.extract_state, ExtractState::Failed);
    assert_eq!(
        big.extract_error.as_deref(),
        Some(zam_store::attachments::TOO_LARGE)
    );
    assert_eq!(active_job(&env.db, "attachment", big.id).await, None);
    // неподдерживаемый вид больше предела — по-прежнему `unsupported`
    put_attach_max(&env, n, b"PNG big", "cat.png", None, 4).await;
    let png = doc_of(&list_attachments(&env.db, n).await.unwrap(), "cat.png");
    assert_eq!(png.extract_state, ExtractState::Unsupported);

    let sha = put_attach(&env, n, b"first", "first.txt", Some(DocKind::Text)).await;
    put_attach(&env, n, b"second", "second.txt", Some(DocKind::Text)).await;
    let names = |l: Vec<AttachmentDoc>| l.into_iter().map(|d| d.filename).collect::<Vec<_>>();
    assert_eq!(
        names(list_attachments(&env.db, n).await.unwrap()),
        ["big.pdf", "cat.png", "first.txt", "second.txt"]
    );
    detach(&env.db, n, &sha, "first.txt", &human())
        .await
        .unwrap();
    put_attach(&env, n, b"first", "first.txt", Some(DocKind::Text)).await;
    assert_eq!(
        names(list_attachments(&env.db, n).await.unwrap()),
        ["big.pdf", "cat.png", "second.txt", "first.txt"]
    );
}

/// Кэш `blob_extract` хранит `units` сжатыми zstd (спецификация M5 §3, раунд правок 1); строка,
/// которая не разжимается, — промах кэша.
#[tokio::test]
async fn extract_cache_is_zstd_and_bad_row_is_miss() {
    let env = setup("zstd").await;
    let sha = "f".repeat(64);
    let long = "повтор ".repeat(2000);
    let ex = Extracted {
        kind: DocKind::Text,
        units: units(&[("", long.as_str())]),
        warnings: Vec::new(),
        ocr_needed: false,
    };
    put_extract(&env.db, &sha, &ex).await.unwrap();
    let raw: Vec<u8> = env
        .db
        .read(|c| Ok(c.query_row("SELECT units FROM blob_extract", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert!(raw.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]), "не кадр zstd");
    assert!(raw.len() < 2000, "не сжато: {} байт", raw.len());
    assert_eq!(extract_cached(&env.db, &sha).await.unwrap(), Some(ex));
    env.db
        .write(|t| {
            t.execute("UPDATE blob_extract SET units = X'7B7D'", [])?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(extract_cached(&env.db, &sha).await.unwrap(), None);
}
