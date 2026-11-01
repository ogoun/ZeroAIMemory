//! Вложения как цель индексации (задача 4 M5): `attachment_docs`, кэш `blob_extract`, чанки
//! вложений (`chunks.attachment_id`), изоляция от чанков заметки и каскад операций над узлом
//! (Review Focus 2 и 3 плана M5), миграция 0019 с базы M4.5.

use std::path::PathBuf;

use zam_core::{Id, TargetKind};
use zam_store::attachments::{
    AttachmentDoc, DocIntake, DocKind, ExtractState, Extracted, Locator, Unit,
    attachment_chunks_for_index, detach, ensure_extract_jobs, extract_cached, list_attachments,
    put_extract, replace_attachment_chunks, set_extract_state,
};
use zam_store::blobs::{AttachScope, BlobStore, attach, resolve_attachment};
use zam_store::{
    Attribution, AuthorKind, DataDir, Db, FtsQuery, NewNote, NodePath, SpaceKind, USER_MIGRATIONS,
    UserDbs, claim_extract_jobs, claim_jobs, create_note, delete_node, list_spaces, move_node,
    restore_node, search_fts, update_note,
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
/// `unsupported` без задания; повтор пары задание не дублирует. Задание извлечения выдаётся
/// конвейеру документов (`claim_extract_jobs`), а не воркеру векторного индекса; после записи
/// чанков документ `done`, задание точек — воркеру.
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
    let now = jiff::Timestamp::now();
    assert!(
        claim_jobs(&env.db, 10, now).await.unwrap().is_empty(),
        "извлечение — не задание воркера векторного индекса"
    );
    let jobs = claim_extract_jobs(&env.db, 10, now).await.unwrap();
    assert_eq!(jobs.len(), 1, "{jobs:?}");
    assert_eq!(jobs[0].target_kind, TargetKind::Attachment);
    assert_eq!(jobs[0].target_id, pdf.id);
    assert_eq!(jobs[0].target().to_string(), format!("d:{}", pdf.id));
    drop_jobs(&env.db).await;

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
    // а у Knowledge — ставят (точки Qdrant): документ `done`, задание — воркеру точек
    replace_attachment_chunks(&env.db, pdf.id, &units(&[("", "текст pdf")]))
        .await
        .unwrap();
    assert_eq!(
        active_job(&env.db, "attachment", pdf.id).await.as_deref(),
        Some("upsert")
    );
    let pdf_doc = doc_of(&list_attachments(&env.db, n).await.unwrap(), "a.pdf");
    assert_eq!(pdf_doc.extract_state, ExtractState::Done);
    let now = jiff::Timestamp::now();
    assert!(
        claim_extract_jobs(&env.db, 10, now)
            .await
            .unwrap()
            .is_empty()
    );
    let jobs = claim_jobs(&env.db, 10, now).await.unwrap();
    assert_eq!(jobs.len(), 1, "{jobs:?}");
    assert_eq!(jobs[0].target_id, pdf.id);
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
    // изоляция в поиске: слово только во вложении — хит документа вложения (задача 6), не
    // заметки; слово заметки находится заметкой
    let hits = search_fts(&env.db, fts("уникальноеслововложения", &[env.k]))
        .await
        .unwrap();
    assert!(!hits.is_empty());
    assert!(hits.iter().all(|h| h.target.kind == TargetKind::Attachment
        && h.target.id == d.id
        && h.level == "attachment"));
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
    // запись чанков — `done` с `ocr_needed` из кэша извлечения (у каждого документа)
    for (node, d) in [(a, da.id), (b, db_.id)] {
        let d = list_attachments(&env.db, node)
            .await
            .unwrap()
            .into_iter()
            .find(|x| x.id == d)
            .unwrap();
        assert_eq!(d.extract_state, ExtractState::Done);
        assert!(d.ocr_needed);
    }
    // `set_extract_state(done)` тоже берёт `ocr_needed` из кэша
    set_extract_state(&env.db, da.id, ExtractState::Done, None)
        .await
        .unwrap();
    let da = doc_of(&list_attachments(&env.db, a).await.unwrap(), "x.txt");
    assert!(da.ocr_needed);

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

/// Задания документов `pending` (задача 5 M5): запуск конвейера ставит их документам без
/// задания; смена GUID решения — тоже (не только извлечённым); reindex возвращает неудавшиеся в
/// `pending` с заданием, кроме `too_large`.
#[tokio::test]
async fn pending_docs_get_jobs_on_start_guid_change_and_reindex() {
    let env = setup("pending-jobs").await;
    let guid = zam_core::SolutionGuid::new_v7();
    let (s, _) = zam_store::create_solution(&env.db, &guid.to_string(), "S")
        .await
        .unwrap();
    let n = note(&env.db, s.id, "n", "текст").await;
    put_attach(&env, n, b"pending text", "p.txt", Some(DocKind::Text)).await;
    put_attach_max(
        &env,
        n,
        b"too large text",
        "big.txt",
        Some(DocKind::Text),
        4,
    )
    .await;
    let d = doc_of(&list_attachments(&env.db, n).await.unwrap(), "p.txt");
    drop_jobs(&env.db).await;
    assert_eq!(ensure_extract_jobs(&env.db).await.unwrap(), 1);
    assert_eq!(
        active_job(&env.db, "attachment", d.id).await.as_deref(),
        Some("upsert")
    );
    assert_eq!(
        ensure_extract_jobs(&env.db).await.unwrap(),
        0,
        "уже с заданием"
    );
    drop_jobs(&env.db).await;
    zam_store::rename_solution_guid(&env.db, s.id, &zam_core::SolutionGuid::new_v7())
        .await
        .unwrap();
    assert_eq!(
        active_job(&env.db, "attachment", d.id).await.as_deref(),
        Some("upsert"),
        "смена GUID: pending — тоже задание"
    );
    drop_jobs(&env.db).await;
    set_extract_state(&env.db, d.id, ExtractState::Failed, Some("parse_crash"))
        .await
        .unwrap();
    zam_store::enqueue_reindex(&env.db, None).await.unwrap();
    let docs = list_attachments(&env.db, n).await.unwrap();
    assert_eq!(doc_of(&docs, "p.txt").extract_state, ExtractState::Pending);
    assert_eq!(
        active_job(&env.db, "attachment", d.id).await.as_deref(),
        Some("upsert")
    );
    let big = doc_of(&docs, "big.txt");
    assert_eq!(
        (big.extract_state, big.extract_error.as_deref()),
        (ExtractState::Failed, Some("too_large"))
    );
    assert_eq!(active_job(&env.db, "attachment", big.id).await, None);
}

/// Чанки вложения для векторного индекса: `level = attachment`, payload документа (`node_id`
/// владельца, `attachment_id`, `blob_sha256`, `locator`); единица длиннее `chunk_hard_max`
/// режется по предложениям с общим `locator`; Human и неизвлечённый документ — без чанков
/// (точек нет).
#[tokio::test]
async fn attachment_chunks_for_index_payload_split_and_human_guard() {
    let env = setup("for-index").await;
    let n = note(&env.db, env.k, "ops/n", "текст").await;
    let sha = put_attach(&env, n, b"pdf", "doc.pdf", Some(DocKind::Pdf)).await;
    let d = doc_of(&list_attachments(&env.db, n).await.unwrap(), "doc.pdf");
    assert!(
        attachment_chunks_for_index(&env.db, d.id)
            .await
            .unwrap()
            .is_empty(),
        "не извлечён — точек нет"
    );
    let long: String = (0..400)
        .map(|i| format!("Предложение {i} длинной страницы документа о вложениях. "))
        .collect();
    let page = vec![Unit {
        locator: Locator::Page(7),
        heading_path: Vec::new(),
        text: long,
    }];
    replace_attachment_chunks(&env.db, d.id, &page)
        .await
        .unwrap();
    let ch = attachment_chunks_for_index(&env.db, d.id).await.unwrap();
    assert!(ch.len() > 2, "длинная страница нарезана: {}", ch.len());
    for c in &ch {
        assert_eq!(c.level, zam_markdown::ChunkLevel::Attachment);
        assert!(c.token_count <= zam_store::chunk_params().hard_max as i64);
        assert!(c.text.starts_with("Предложение"), "{}", c.text);
        assert_eq!(c.node.node_id, n);
        assert_eq!(c.node.path_ancestors, ["ops", "ops/n"]);
        let a = c.attachment.as_ref().unwrap();
        assert_eq!(a.attachment_id, d.id);
        assert_eq!(a.blob_sha256, sha);
        assert_eq!(a.locator.as_deref(), Some("page:7"));
    }
    // Human: чанки (FTS) есть, для векторного индекса — нет
    let hn = note(&env.db, env.h, "личное", "текст").await;
    put_attach(&env, hn, b"human", "h.txt", Some(DocKind::Text)).await;
    let hd = doc_of(&list_attachments(&env.db, hn).await.unwrap(), "h.txt");
    replace_attachment_chunks(&env.db, hd.id, &units(&[("", "человеческое")]))
        .await
        .unwrap();
    assert_eq!(
        of_attachment(&chunks_of(&env.db, hn).await, hd.id).len(),
        1,
        "FTS"
    );
    assert!(
        attachment_chunks_for_index(&env.db, hd.id)
            .await
            .unwrap()
            .is_empty()
    );
}

/// Ревью задачи 5 (M-1, M-2, I-1): итог извлечения — одной транзакцией с заданием
/// (`finish_extract`); повтор после reindex и новая неудача оставляют в `failed_jobs` одну строку
/// документа; `done` с той же нарезкой после `pending` ставит задание точек.
#[tokio::test]
async fn finish_extract_one_failed_row_and_points_after_same_chunks() {
    use zam_store::{ExtractEnd, JobOutcome, enqueue_reindex, failed_jobs, finish_extract};
    let env = setup("finish").await;
    let n = note(&env.db, env.k, "n", "текст").await;
    put_attach(&env, n, b"doc", "d.txt", Some(DocKind::Text)).await;
    let d = doc_of(&list_attachments(&env.db, n).await.unwrap(), "d.txt");
    for round in 0..3 {
        let jobs = claim_extract_jobs(&env.db, 10, jiff::Timestamp::now())
            .await
            .unwrap();
        assert_eq!(jobs.len(), 1, "раунд {round}: {jobs:?}");
        let o = finish_extract(
            &env.db,
            jobs[0].id,
            jobs[0].claim_gen,
            d.id,
            ExtractEnd::Failed("parse_error: xml (word/document.xml)".into()),
        )
        .await
        .unwrap();
        assert_eq!(o, JobOutcome::Failed);
        let mine: Vec<_> = failed_jobs(&env.db, 100)
            .await
            .unwrap()
            .into_iter()
            .filter(|f| f.node_id == d.id)
            .collect();
        assert_eq!(mine.len(), 1, "раунд {round}: одна строка неудачи");
        assert_eq!(mine[0].reason.as_deref(), Some("parse_error"));
        let st = doc_of(&list_attachments(&env.db, n).await.unwrap(), "d.txt");
        assert_eq!(st.extract_state, ExtractState::Failed);
        enqueue_reindex(&env.db, None).await.unwrap();
    }
    // успех снимает неудачи; та же нарезка после `pending` — задание точек
    let units = units(&[("", "текст документа")]);
    let jobs = claim_extract_jobs(&env.db, 10, jiff::Timestamp::now())
        .await
        .unwrap();
    finish_extract(
        &env.db,
        jobs[0].id,
        jobs[0].claim_gen,
        d.id,
        ExtractEnd::Units(units.clone()),
    )
    .await
    .unwrap();
    assert!(failed_jobs(&env.db, 100).await.unwrap().is_empty());
    drop_jobs(&env.db).await;
    // документ снова `pending` (перенарезка), чанки прежние
    env.db
        .write(move |t| {
            t.execute(
                "UPDATE attachment_docs SET extract_state = 'pending' WHERE id = ?1",
                [d.id.to_string()],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    replace_attachment_chunks(&env.db, d.id, &units)
        .await
        .unwrap();
    assert_eq!(
        active_job(&env.db, "attachment", d.id).await.as_deref(),
        Some("upsert"),
        "та же нарезка после pending — задание точек"
    );
    drop_jobs(&env.db).await;
    replace_attachment_chunks(&env.db, d.id, &units)
        .await
        .unwrap();
    assert_eq!(
        active_job(&env.db, "attachment", d.id).await,
        None,
        "done и та же нарезка — без задания"
    );
}

/// Ревью задачи 5, раунд 2 (m-3): полный reindex — способ повтора разбора: закэшированные
/// неудачи блобов (`blob_extract` `failed`/`unsupported`) снимаются, документы `failed` и
/// `unsupported` снова `pending` с заданием; `too_large` остаётся.
#[tokio::test]
async fn full_reindex_drops_cached_failures() {
    use zam_store::{CachedExtract, cached_extract, enqueue_reindex, put_extract_failure};
    let env = setup("reindex-failures").await;
    let n = note(&env.db, env.k, "n", "текст").await;
    let bad = put_attach(&env, n, b"bad docx", "bad.docx", Some(DocKind::Docx)).await;
    let odd = put_attach(&env, n, b"odd", "odd.xlsx", Some(DocKind::Xlsx)).await;
    put_attach_max(
        &env,
        n,
        b"too large text",
        "big.txt",
        Some(DocKind::Text),
        4,
    )
    .await;
    put_extract_failure(&env.db, &bad, Some("parse_error: xml (word/document.xml)"))
        .await
        .unwrap();
    put_extract_failure(&env.db, &odd, None).await.unwrap();
    assert_eq!(
        cached_extract(&env.db, &bad).await.unwrap(),
        Some(CachedExtract::Failed(
            "parse_error: xml (word/document.xml)".into()
        ))
    );
    assert_eq!(
        cached_extract(&env.db, &odd).await.unwrap(),
        Some(CachedExtract::Unsupported)
    );
    let docs = list_attachments(&env.db, n).await.unwrap();
    let (b, o) = (doc_of(&docs, "bad.docx").id, doc_of(&docs, "odd.xlsx").id);
    set_extract_state(&env.db, b, ExtractState::Failed, Some("parse_error: xml"))
        .await
        .unwrap();
    set_extract_state(&env.db, o, ExtractState::Unsupported, None)
        .await
        .unwrap();
    drop_jobs(&env.db).await;
    enqueue_reindex(&env.db, None).await.unwrap();
    assert_eq!(cached_extract(&env.db, &bad).await.unwrap(), None);
    assert_eq!(cached_extract(&env.db, &odd).await.unwrap(), None);
    let docs = list_attachments(&env.db, n).await.unwrap();
    for name in ["bad.docx", "odd.xlsx"] {
        let d = doc_of(&docs, name);
        assert_eq!(d.extract_state, ExtractState::Pending, "{name}");
        assert_eq!(
            active_job(&env.db, "attachment", d.id).await.as_deref(),
            Some("upsert"),
            "{name}"
        );
    }
    let big = doc_of(&docs, "big.txt");
    assert_eq!(big.extract_error.as_deref(), Some("too_large"));
}

/// Задача 6 M5: чанки вложения выдаются каналами FTS (`bm25`, `trigram`, `alias`) и
/// [`live_chunks`] как цель-документ с файлом и `locator`, с `area`/`certainty` узла-владельца;
/// виды и фильтры (`kinds`, `area`, `certainty`) действуют на них. Чанки скрытого документа
/// (метка `attachment_docs.deleted_at` при живом узле; удалённый узел — чанки остаются в FTS) не
/// выдаёт ни один канал; листы и соседи заметки чанков вложения не содержат.
#[tokio::test]
async fn search_channels_hide_hidden_attachment_docs() {
    use zam_core::TargetRef;
    use zam_store::{
        AliasGroup, ChunkScope, TargetFilter, leaves_of_targets, live_chunks, neighbor_leaves,
        search_fts_channels,
    };
    let env = setup("search-hidden").await;
    let n = note(
        &env.db,
        env.k,
        "proj/хозяин",
        "---\ncertainty: hypothesis\n---\n# Хозяин\n\nПросто погода.\n\n## Второй\n\nЕщё погода.\n",
    )
    .await;
    let sha = put_attach(&env, n, b"doc", "doc.txt", Some(DocKind::Text)).await;
    let d = doc_of(&list_attachments(&env.db, n).await.unwrap(), "doc.txt");
    set_extract_state(&env.db, d.id, ExtractState::Done, None)
        .await
        .unwrap();
    replace_attachment_chunks(
        &env.db,
        d.id,
        &units(&[
            ("Раздел", "Плутоний и метка zx_4242"),
            ("", "второй фрагмент"),
        ]),
    )
    .await
    .unwrap();
    let doc_target = TargetRef {
        kind: TargetKind::Attachment,
        id: d.id,
    };
    let q = |text: &str, targets: TargetFilter, groups: Vec<AliasGroup>| FtsQuery {
        alias_groups: groups,
        targets,
        ..fts(text, &[env.k])
    };
    let alias = || {
        vec![AliasGroup {
            start: 0,
            len: 1,
            with: vec!["плутоний".into()],
        }]
    };
    // (bm25, trigram, alias) — число хитов документа
    let channels = |text: &'static str, targets: TargetFilter| {
        let db = env.db.clone();
        let qq = q(text, targets, alias());
        async move {
            let ch = search_fts_channels(&db, qq).await.unwrap();
            let of = |l: &[zam_store::FtsHit]| l.iter().filter(|h| h.target == doc_target).count();
            (of(&ch.bm25), of(&ch.trigram), of(&ch.alias))
        }
    };
    let kinds = |k: &[&str]| TargetFilter {
        kinds: k.iter().map(|s| s.to_string()).collect(),
        ..TargetFilter::default()
    };

    // живой документ: все каналы
    let ch = search_fts_channels(&env.db, q("плутоний", TargetFilter::default(), Vec::new()))
        .await
        .unwrap();
    assert_eq!(ch.bm25.len(), 1, "{:?}", ch.bm25);
    let h = &ch.bm25[0];
    assert_eq!(h.target, doc_target);
    assert_eq!(h.level, "attachment");
    assert_eq!(h.path, "proj/хозяин");
    assert_eq!(h.certainty.as_deref(), Some("hypothesis"));
    let att = h.attachment.as_ref().expect("поля вложения");
    assert_eq!((att.node_id, att.filename.as_str()), (n, "doc.txt"));
    assert_eq!(att.blob_sha256, sha);
    assert_eq!(att.locator.as_deref(), Some("para:1"));
    let chunk = h.chunk_id.clone();
    assert_eq!(channels("плутоний", TargetFilter::default()).await.0, 1);
    assert_eq!(channels("zx_4242", TargetFilter::default()).await.1, 1);
    assert_eq!(
        channels("радиоактивный", TargetFilter::default()).await.2,
        1
    );
    assert_eq!(channels("плутоний", kinds(&["attachment"])).await.0, 1);
    assert_eq!(
        channels("плутоний", kinds(&["note", "insight"])).await,
        (0, 0, 0)
    );
    let cert = |c: &str| TargetFilter {
        certainty: vec![c.into()],
        ..TargetFilter::default()
    };
    assert_eq!(channels("плутоний", cert("hypothesis")).await.0, 1);
    assert_eq!(channels("плутоний", cert("axiom")).await.0, 0);
    let area = TargetFilter {
        area: Some("другая".into()),
        ..TargetFilter::default()
    };
    assert_eq!(channels("плутоний", area).await.0, 0);
    let scope = || ChunkScope {
        space_ids: vec![env.k],
        path_prefix: None,
        indexable_only: true,
        targets: TargetFilter::default(),
    };
    let live = live_chunks(&env.db, vec![chunk.clone()], scope())
        .await
        .unwrap();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].attachment.as_ref().unwrap().filename, "doc.txt");
    let only_notes = ChunkScope {
        targets: kinds(&["note"]),
        ..scope()
    };
    assert!(
        live_chunks(&env.db, vec![chunk.clone()], only_notes)
            .await
            .unwrap()
            .is_empty()
    );
    // листы и соседи узла — без чанков вложения
    let node_t = TargetRef {
        kind: TargetKind::Node,
        id: n,
    };
    let leaves = leaves_of_targets(&env.db, vec![node_t], true)
        .await
        .unwrap();
    let leaves = &leaves[&node_t];
    assert!(
        leaves.iter().all(|l| !l.text.contains("Плутоний")),
        "{leaves:?}"
    );
    for l in leaves {
        let (p, nx) = neighbor_leaves(&env.db, l.chunk_id.clone()).await.unwrap();
        for t in [p, nx].into_iter().flatten() {
            assert!(
                !t.contains("Плутоний") && !t.contains("второй фрагмент"),
                "{t}"
            );
        }
    }
    assert_eq!(
        neighbor_leaves(&env.db, chunk.clone()).await.unwrap(),
        (None, None)
    );

    // документ скрыт меткой при живом узле
    let id = d.id.to_string();
    let hide = |v: Option<&'static str>| {
        let id = id.clone();
        env.db.write(move |t| {
            t.execute(
                "UPDATE attachment_docs SET deleted_at = ?2 WHERE id = ?1",
                rusqlite::params![id, v],
            )?;
            Ok(())
        })
    };
    hide(Some(TS)).await.unwrap();
    for text in ["плутоний", "zx_4242", "радиоактивный"] {
        assert_eq!(
            channels(text, TargetFilter::default()).await,
            (0, 0, 0),
            "{text}"
        );
    }
    assert!(
        live_chunks(&env.db, vec![chunk.clone()], scope())
            .await
            .unwrap()
            .is_empty()
    );
    hide(None).await.unwrap();
    assert_eq!(channels("плутоний", TargetFilter::default()).await.0, 1);

    // узел удалён: чанки вложения остаются в FTS, но не выдаются
    delete_node(&env.db, n, AuthorKind::Human).await.unwrap();
    assert_eq!(of_attachment(&chunks_of(&env.db, n).await, d.id).len(), 2);
    for text in ["плутоний", "zx_4242", "радиоактивный"] {
        assert_eq!(
            channels(text, TargetFilter::default()).await,
            (0, 0, 0),
            "{text}"
        );
    }
    assert!(
        live_chunks(&env.db, vec![chunk], scope())
            .await
            .unwrap()
            .is_empty()
    );
}

/// Раунд 1 задачи 6 M5 (M-4): `index_state` хита вложения — документа (а не заметки-владельца):
/// `pending`, пока ждёт извлечения или задания точек, `indexed` — без заданий в очереди;
/// `pending_count` учитывает документы, ждущие извлечения или точек (Human — нет).
#[tokio::test]
async fn attachment_index_state_and_pending_count() {
    use zam_store::{TargetFilter, pending_count, search_fts_channels};
    let env = setup("index-state").await;
    let n = note(&env.db, env.k, "p/x", "# X\n\nТекст.\n").await;
    drop_jobs(&env.db).await;
    // заметка без заданий — indexed (имитация воркера)
    env.db
        .write(move |t| {
            t.execute(
                "UPDATE nodes SET index_state = 'indexed' WHERE id = ?1",
                [n],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(pending_count(&env.db, vec![env.k], None).await.unwrap(), 0);
    put_attach(&env, n, b"doc", "doc.txt", Some(DocKind::Text)).await;
    // документ ждёт извлечения
    assert_eq!(pending_count(&env.db, vec![env.k], None).await.unwrap(), 1);
    let d = doc_of(&list_attachments(&env.db, n).await.unwrap(), "doc.txt");
    set_extract_state(&env.db, d.id, ExtractState::Done, None)
        .await
        .unwrap();
    replace_attachment_chunks(&env.db, d.id, &units(&[("", "иттербий")]))
        .await
        .unwrap();
    let state = || async {
        let q = FtsQuery {
            targets: TargetFilter::default(),
            ..fts("иттербий", &[env.k])
        };
        let ch = search_fts_channels(&env.db, q).await.unwrap();
        ch.bm25[0].index_state.clone()
    };
    // извлечён, задание точек в очереди — ещё не виден вектору
    assert_eq!(
        active_job(&env.db, "attachment", d.id).await.as_deref(),
        Some("upsert")
    );
    assert_eq!(state().await, "pending");
    assert_eq!(pending_count(&env.db, vec![env.k], None).await.unwrap(), 1);
    drop_jobs(&env.db).await;
    assert_eq!(state().await, "indexed");
    assert_eq!(pending_count(&env.db, vec![env.k], None).await.unwrap(), 0);
    // Human-вложение в Qdrant не попадает — в pending_count его нет
    let h = note(&env.db, env.h, "h/y", "# Y\n").await;
    let before = pending_count(&env.db, vec![env.h], None).await.unwrap();
    put_attach(&env, h, b"hdoc", "h.txt", Some(DocKind::Text)).await;
    assert_eq!(
        pending_count(&env.db, vec![env.h], None).await.unwrap(),
        before
    );
}

/// Раунд правок 2 задачи 6 M5: `has_extracted_attachments` — есть ли в пространствах извлечённый
/// живой документ живой заметки (иначе поиск не запускает prefetch вложений в Qdrant).
#[tokio::test]
async fn has_extracted_attachments_by_space() {
    use zam_store::has_extracted_attachments;
    let env = setup("has-extracted").await;
    assert!(
        !has_extracted_attachments(&env.db, vec![env.k])
            .await
            .unwrap()
    );
    assert!(
        !has_extracted_attachments(&env.db, Vec::new())
            .await
            .unwrap()
    );
    let n = note(&env.db, env.k, "p/z", "# Z\n").await;
    put_attach(&env, n, b"doc", "doc.txt", Some(DocKind::Text)).await;
    // ещё не извлечён
    assert!(
        !has_extracted_attachments(&env.db, vec![env.k])
            .await
            .unwrap()
    );
    let d = doc_of(&list_attachments(&env.db, n).await.unwrap(), "doc.txt");
    set_extract_state(&env.db, d.id, ExtractState::Done, None)
        .await
        .unwrap();
    assert!(
        has_extracted_attachments(&env.db, vec![env.k])
            .await
            .unwrap()
    );
    // другое пространство — нет
    assert!(
        !has_extracted_attachments(&env.db, vec![env.h])
            .await
            .unwrap()
    );
    // узел удалён — документ скрыт
    delete_node(&env.db, n, AuthorKind::Human).await.unwrap();
    assert!(
        !has_extracted_attachments(&env.db, vec![env.k])
            .await
            .unwrap()
    );
}
