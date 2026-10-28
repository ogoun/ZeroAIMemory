//! Записи журналов (§7.2–7.5 ТЗ, задача 6 M3): хранение, ревизии, чанки и FTS, задания
//! индексации `target_kind = 'entry'`, замещение и отзыв, лента с курсором, миграция v7 → v8.

use std::path::PathBuf;

use jiff::Timestamp;
use serde_json::json;
use zam_core::{Id, TargetKind};
use zam_store::{
    Attribution, AuthorKind, Certainty, DataDir, Db, EntryFilter, EntryKind, EntryPatch,
    EntryStatus, FtsQuery, IndexState, JobOp, JobOutcome, NewEntry, NewTrigger, SpaceKind,
    StoreError, TargetFilter, TriggerKind, USER_MIGRATIONS, UserDbs, active_triggers, claim_jobs,
    complete_job, create_entry, delete_entry, entry_chunks_for_index, entry_revisions,
    entry_triggers, get_entry, list_entries, list_spaces, normalize_title, restore_entry,
    retract_entry, search_fts, set_triggers, supersede_entry, title_duplicates, update_entry,
};

fn datadir(name: &str) -> DataDir {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("entries-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    DataDir::open(&dir).unwrap()
}

/// База пользователя, id пространств Knowledge и Human.
async fn setup(name: &str) -> (Db, Id, Id) {
    let dd = datadir(name);
    let db = UserDbs::new(dd.users_dir()).get(Id::new()).await.unwrap();
    let spaces = list_spaces(&db).await.unwrap();
    let find = |k: SpaceKind| spaces.iter().find(|s| s.kind == k).unwrap().id;
    (db, find(SpaceKind::Knowledge), find(SpaceKind::Human))
}

fn agent() -> Attribution {
    Attribution {
        author_kind: AuthorKind::Agent,
        token_id: Some(Id::new()),
        session_id: Some("s1".into()),
        machine: Some("box".into()),
        message: None,
        user_id: Some(Id::new()),
        area: None,
    }
}

fn insight(space: Id, title: &str, body: &str) -> NewEntry {
    NewEntry {
        space_id: space,
        kind: EntryKind::Insight,
        title: title.into(),
        body: body.into(),
        data: json!({"kind": "gotcha"}),
        area: None,
        tags: vec!["bash".into(), "line-endings".into()],
        status: None,
        certainty: Some(Certainty::Hypothesis),
        idempotency_key: None,
        triggers: Vec::new(),
        links: Vec::new(),
    }
}

fn changelog(space: Id, title: &str) -> NewEntry {
    NewEntry {
        space_id: space,
        kind: EntryKind::Changelog,
        title: title.into(),
        body: "Подробности изменения.".into(),
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

/// `(op, state, rev)` заданий записи по порядку id.
async fn jobs_of(db: &Db, id: Id) -> Vec<(String, String, i64)> {
    db.read(move |c| {
        let mut st = c.prepare(
            "SELECT op, state, rev FROM index_jobs
             WHERE target_kind = 'entry' AND target_id = ?1 ORDER BY id",
        )?;
        let rows = st.query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    })
    .await
    .unwrap()
}

/// `(level, context_prefix, rev)` чанков записи по порядку.
async fn chunks_of(db: &Db, id: Id) -> Vec<(String, String, i64)> {
    db.read(move |c| {
        let mut st = c.prepare(
            "SELECT level, context_prefix, rev FROM chunks
             WHERE entry_id = ?1 AND node_id IS NULL ORDER BY ordinal",
        )?;
        let rows = st.query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    })
    .await
    .unwrap()
}

fn query(text: &str, space: Id, targets: TargetFilter) -> FtsQuery {
    FtsQuery {
        text: text.into(),
        space_ids: vec![space],
        path_prefix: None,
        top_k: 20,
        targets,
    }
}

/// Цели хитов FTS по запросу.
async fn fts(db: &Db, text: &str, space: Id, targets: TargetFilter) -> Vec<Id> {
    search_fts(db, query(text, space, targets))
        .await
        .unwrap()
        .into_iter()
        .map(|h| h.target.id)
        .collect()
}

#[tokio::test]
async fn create_entry_chunks_fts_and_job() {
    let (db, k, h) = setup("create").await;
    let mut a = agent();
    a.area = Some("web".into());
    let (e, created) = create_entry(
        &db,
        insight(
            k,
            "Bash-скрипты должны иметь LF",
            "Иначе интерпретатор падает: bad interpreter.",
        ),
        &a,
    )
    .await
    .unwrap();
    assert!(created);
    assert_eq!(e.kind, EntryKind::Insight);
    assert_eq!(
        e.status,
        Some(EntryStatus::Active),
        "insight по умолчанию active"
    );
    assert_eq!(e.certainty, Some(Certainty::Hypothesis));
    assert_eq!(e.rev, 1);
    assert_eq!(e.area.as_deref(), Some("web"), "область из X-Zam-Area");
    assert_eq!(e.index_state, IndexState::Pending);
    assert_eq!(e.author.kind, AuthorKind::Agent);
    assert_eq!(e.author.user_id, a.user_id);
    assert_eq!(e.author.machine.as_deref(), Some("box"));
    let title_norm: String = db
        .read(move |c| {
            Ok(c.query_row(
                "SELECT title_norm FROM entries WHERE id = ?1",
                [e.id],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(title_norm, normalize_title("Bash-скрипты должны иметь LF"));

    // чанки: префикс `[<пространство>/<kind>] <title> | <теги>`, листы и L1 записи
    let ch = chunks_of(&db, e.id).await;
    assert!(!ch.is_empty());
    assert!(ch.iter().any(|c| c.0 == "chunk"), "{ch:?}");
    assert!(
        ch.iter().all(|c| c.1
            == "[Knowledge/insight] Bash-скрипты должны иметь LF | bash, line-endings"
            && c.2 == 1),
        "{ch:?}"
    );
    // задание upsert в той же транзакции
    assert_eq!(
        jobs_of(&db, e.id).await,
        vec![("upsert".into(), "queued".into(), 1)]
    );
    // FTS находит запись по телу и по заголовку
    assert_eq!(
        fts(&db, "interpreter", k, TargetFilter::default()).await,
        vec![e.id]
    );
    assert_eq!(
        fts(&db, "скрипты", k, TargetFilter::default()).await,
        vec![e.id]
    );
    // фильтр видов: только заметки — записи нет
    let notes = TargetFilter {
        kinds: vec!["note".into()],
        ..TargetFilter::default()
    };
    assert!(fts(&db, "interpreter", k, notes).await.is_empty());
    // хит несёт поля записи
    let hit = search_fts(&db, query("interpreter", k, TargetFilter::default()))
        .await
        .unwrap()
        .remove(0);
    assert_eq!(hit.target.kind, TargetKind::Entry);
    let eh = hit.entry.unwrap();
    assert_eq!(
        (
            eh.kind.as_str(),
            eh.status.as_deref(),
            eh.insight_kind.as_deref()
        ),
        ("insight", Some("active"), Some("gotcha"))
    );
    assert_eq!(eh.certainty.as_deref(), Some("hypothesis"));
    assert_eq!(eh.title, "Bash-скрипты должны иметь LF");

    // воркер получает задание записи и её чанки с payload
    let jobs = claim_jobs(&db, 10, Timestamp::now()).await.unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].target_kind, TargetKind::Entry);
    assert_eq!(jobs[0].target_id, e.id);
    let ic = entry_chunks_for_index(&db, e.id).await.unwrap();
    assert!(!ic.is_empty());
    let pl = ic[0].entry.clone().unwrap();
    assert_eq!(pl.entry_id, e.id);
    assert_eq!(
        (
            pl.kind.as_str(),
            pl.status.as_deref(),
            pl.insight_kind.as_deref(),
            pl.certainty.as_deref(),
            pl.area.as_deref()
        ),
        (
            "insight",
            Some("active"),
            Some("gotcha"),
            Some("hypothesis"),
            Some("web")
        )
    );
    assert_eq!(ic[0].node.space_id, k);
    assert!(ic[0].node.path_ancestors.is_empty());
    assert_eq!(
        complete_job(&db, jobs[0].id, jobs[0].claim_gen)
            .await
            .unwrap(),
        JobOutcome::Done
    );
    assert_eq!(
        get_entry(&db, e.id).await.unwrap().index_state,
        IndexState::Indexed
    );

    // Human никогда не векторизуется: excluded, без задания, чанков для индекса нет
    let (hu, _) = create_entry(&db, changelog(h, "Личное изменение"), &agent())
        .await
        .unwrap();
    assert_eq!(hu.index_state, IndexState::Excluded);
    assert!(jobs_of(&db, hu.id).await.is_empty());
    assert!(entry_chunks_for_index(&db, hu.id).await.unwrap().is_empty());

    // целостность
    let mut bad = changelog(k, "x");
    bad.status = Some(EntryStatus::Active);
    assert!(matches!(
        create_entry(&db, bad, &agent()).await,
        Err(StoreError::BadInput(_))
    ));
    let mut bad = changelog(k, "x");
    bad.certainty = Some(Certainty::Axiom);
    assert!(matches!(
        create_entry(&db, bad, &agent()).await,
        Err(StoreError::BadInput(_))
    ));
    let mut bad = insight(k, "x", "");
    bad.status = Some(EntryStatus::Superseded);
    assert!(matches!(
        create_entry(&db, bad, &agent()).await,
        Err(StoreError::BadInput(_))
    ));
    assert!(matches!(
        create_entry(&db, changelog(k, "   "), &agent()).await,
        Err(StoreError::BadInput(_))
    ));
    let mut bad = changelog(k, "x");
    bad.data = json!([1, 2]);
    assert!(matches!(
        create_entry(&db, bad, &agent()).await,
        Err(StoreError::BadInput(_))
    ));
    assert!(matches!(
        create_entry(&db, changelog(Id::new(), "x"), &agent()).await,
        Err(StoreError::NotFound)
    ));
}

#[tokio::test]
async fn idempotency_key_returns_same_entry() {
    let (db, k, h) = setup("idem").await;
    let me = agent();
    let mut n = changelog(k, "Исправлен перенос строк");
    n.idempotency_key = Some("key-1".into());
    let (a, created) = create_entry(&db, n.clone(), &me).await.unwrap();
    assert!(created);
    let (b, created) = create_entry(&db, n.clone(), &me).await.unwrap();
    assert!(!created);
    assert_eq!(a.id, b.id);
    let count: i64 = db
        .read(|c| Ok(c.query_row("SELECT count(*) FROM entries", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(entry_revisions(&db, a.id).await.unwrap().len(), 1);
    // правка записи не мешает повтору исходного запроса
    let p = EntryPatch {
        body: Some("Другой текст.".into()),
        ..EntryPatch::default()
    };
    update_entry(&db, a.id, None, p, &me).await.unwrap();
    let (c, created) = create_entry(&db, n.clone(), &me).await.unwrap();
    assert!(!created);
    assert_eq!(c.id, a.id);
    // тот же ключ при другом пространстве, заголовке, теле, данных или виде — расхождение
    let mut other_space = n.clone();
    other_space.space_id = h;
    let mut other_title = n.clone();
    other_title.title = "Иное".into();
    let mut other_body = n.clone();
    other_body.body = "Иное тело".into();
    let mut other_data = n.clone();
    other_data.data = json!({"kind": "feat"});
    let mut other_kind = n.clone();
    other_kind.kind = EntryKind::Release;
    for bad in [other_space, other_title, other_body, other_data, other_kind] {
        assert!(matches!(
            create_entry(&db, bad, &me).await,
            Err(StoreError::IdempotencyMismatch(_))
        ));
    }
}

#[tokio::test]
async fn idempotency_key_is_per_author() {
    let (db, k, _) = setup("idem-author").await;
    let mut n = changelog(k, "Общий заголовок");
    n.idempotency_key = Some("same".into());
    let (a, created_a) = create_entry(&db, n.clone(), &agent()).await.unwrap();
    // другой автор с тем же ключом получает свою запись, а не чужую
    let (b, created_b) = create_entry(&db, n.clone(), &agent()).await.unwrap();
    assert!(created_a && created_b);
    assert_ne!(a.id, b.id);
    // и даже с другим содержимым — расхождения нет
    n.title = "Другой".into();
    let (c, created_c) = create_entry(&db, n, &agent()).await.unwrap();
    assert!(created_c);
    assert_ne!(c.id, b.id);
}

#[tokio::test]
async fn idempotency_key_of_deleted_entry_conflicts() {
    let (db, k, _) = setup("idem-deleted").await;
    let me = agent();
    let mut n = changelog(k, "Удаляемая");
    n.idempotency_key = Some("del-1".into());
    let (a, _) = create_entry(&db, n.clone(), &me).await.unwrap();
    delete_entry(&db, a.id, None, &me).await.unwrap();
    assert!(matches!(
        create_entry(&db, n.clone(), &me).await,
        Err(StoreError::Conflict(_))
    ));
    // после восстановления повтор снова возвращает запись
    restore_entry(&db, a.id, None, &me).await.unwrap();
    let (b, created) = create_entry(&db, n, &me).await.unwrap();
    assert!(!created);
    assert_eq!(b.id, a.id);
}

#[tokio::test]
async fn patch_creates_revision_with_reason_and_enqueues() {
    let (db, k, _) = setup("patch").await;
    let (e, _) = create_entry(&db, insight(k, "Заголовок", "Тело."), &agent())
        .await
        .unwrap();
    // задание создания выполнено
    let jobs = claim_jobs(&db, 10, Timestamp::now()).await.unwrap();
    complete_job(&db, jobs[0].id, jobs[0].claim_gen)
        .await
        .unwrap();
    assert!(jobs_of(&db, e.id).await.is_empty());

    let mut human = Attribution::new(AuthorKind::Human);
    human.machine = Some("desk".into());
    let p = EntryPatch {
        title: Some("Новый заголовок".into()),
        tags: Some(vec!["x".into()]),
        status: Some(EntryStatus::Revisit),
        certainty: Some(Some(Certainty::Theorem)),
        reason: Some("проверено на CI".into()),
        ..EntryPatch::default()
    };
    let u = update_entry(&db, e.id, Some(1), p, &human).await.unwrap();
    assert_eq!(u.rev, 2);
    assert_eq!(u.title, "Новый заголовок");
    assert_eq!(u.status, Some(EntryStatus::Revisit));
    assert_eq!(u.certainty, Some(Certainty::Theorem));
    assert_eq!(u.index_state, IndexState::Pending);
    // автор записи — создатель, автор ревизии — правщик
    assert_eq!(u.author.kind, AuthorKind::Agent);
    let revs = entry_revisions(&db, e.id).await.unwrap();
    assert_eq!(revs.len(), 2);
    assert_eq!(revs[0].title, "Заголовок");
    assert_eq!(revs[0].reason, None);
    assert_eq!(revs[1].rev, 2);
    assert_eq!(revs[1].reason.as_deref(), Some("проверено на CI"));
    assert_eq!(revs[1].status, Some(EntryStatus::Revisit));
    assert_eq!(revs[1].certainty, Some(Certainty::Theorem));
    assert_eq!(revs[1].author.kind, AuthorKind::Human);
    assert_eq!(revs[1].author.machine.as_deref(), Some("desk"));
    // чанки пересобраны по ревизии 2 с новым префиксом, задание upsert
    let ch = chunks_of(&db, e.id).await;
    assert!(
        ch.iter()
            .all(|c| c.1 == "[Knowledge/insight] Новый заголовок | x" && c.2 == 2),
        "{ch:?}"
    );
    assert_eq!(
        jobs_of(&db, e.id).await,
        vec![("upsert".into(), "queued".into(), 2)]
    );

    // смена только certainty — тоже ревизия и upsert (payload)
    let jobs = claim_jobs(&db, 10, Timestamp::now()).await.unwrap();
    complete_job(&db, jobs[0].id, jobs[0].claim_gen)
        .await
        .unwrap();
    let p = EntryPatch {
        certainty: Some(Some(Certainty::Hypothesis)),
        reason: Some("пересмотр".into()),
        ..EntryPatch::default()
    };
    let u = update_entry(&db, e.id, None, p, &agent()).await.unwrap();
    assert_eq!(u.rev, 3);
    assert_eq!(
        jobs_of(&db, e.id).await,
        vec![("upsert".into(), "queued".into(), 3)]
    );

    // правка без изменений — без ревизии
    let p = EntryPatch {
        certainty: Some(Some(Certainty::Hypothesis)),
        ..EntryPatch::default()
    };
    assert_eq!(
        update_entry(&db, e.id, None, p, &agent())
            .await
            .unwrap()
            .rev,
        3
    );
    // устаревшая ревизия — конфликт с текущей ревизией (§11.3); так же у удаления, отзыва,
    // замещения и восстановления
    let p = EntryPatch {
        body: Some("x".into()),
        ..EntryPatch::default()
    };
    let stale = |r: Result<(), StoreError>| {
        assert!(
            matches!(
                r,
                Err(StoreError::StaleRevision {
                    expected: 1,
                    current: 3
                })
            ),
            "{r:?}"
        )
    };
    stale(
        update_entry(&db, e.id, Some(1), p, &agent())
            .await
            .map(drop),
    );
    stale(delete_entry(&db, e.id, Some(1), &agent()).await);
    stale(restore_entry(&db, e.id, Some(1), &agent()).await.map(drop));
    stale(
        retract_entry(&db, e.id, Some(1), "r", &agent())
            .await
            .map(drop),
    );
    stale(
        supersede_entry(&db, e.id, Some(1), insight(k, "Замена", ""), &agent())
            .await
            .map(drop),
    );
    // пустая причина, статус не у insight, superseded правкой — BadInput
    let p = EntryPatch {
        body: Some("x".into()),
        reason: Some("  ".into()),
        ..EntryPatch::default()
    };
    assert!(matches!(
        update_entry(&db, e.id, None, p, &agent()).await,
        Err(StoreError::BadInput(_))
    ));
    let p = EntryPatch {
        status: Some(EntryStatus::Superseded),
        ..EntryPatch::default()
    };
    assert!(matches!(
        update_entry(&db, e.id, None, p, &agent()).await,
        Err(StoreError::BadInput(_))
    ));
    let (c, _) = create_entry(&db, changelog(k, "ch"), &agent())
        .await
        .unwrap();
    let p = EntryPatch {
        status: Some(EntryStatus::Active),
        ..EntryPatch::default()
    };
    assert!(matches!(
        update_entry(&db, c.id, None, p, &agent()).await,
        Err(StoreError::BadInput(_))
    ));
    assert!(matches!(
        update_entry(&db, Id::new(), None, EntryPatch::default(), &agent()).await,
        Err(StoreError::NotFound)
    ));
}

#[tokio::test]
async fn append_body_appends_with_blank_line() {
    let (db, k, _) = setup("append").await;
    let (e, _) = create_entry(&db, changelog(k, "Релиз парсера"), &agent())
        .await
        .unwrap();
    let p = EntryPatch {
        append_body: Some("Дополнение после проверки.".into()),
        ..EntryPatch::default()
    };
    let u = update_entry(&db, e.id, None, p, &agent()).await.unwrap();
    assert_eq!(
        u.body,
        "Подробности изменения.\n\nДополнение после проверки."
    );
    assert_eq!(u.rev, 2);
    // дописанное находится поиском
    assert_eq!(
        fts(&db, "Дополнение", k, TargetFilter::default()).await,
        vec![e.id]
    );
    // body и append_body вместе — BadInput
    let p = EntryPatch {
        body: Some("a".into()),
        append_body: Some("b".into()),
        ..EntryPatch::default()
    };
    assert!(matches!(
        update_entry(&db, e.id, None, p, &agent()).await,
        Err(StoreError::BadInput(_))
    ));
}

#[tokio::test]
async fn entry_revisions_append_only() {
    let (db, k, _) = setup("append-only").await;
    let (e, _) = create_entry(&db, insight(k, "t", "b"), &agent())
        .await
        .unwrap();
    for sql in [
        "UPDATE entry_revisions SET title = 'x' WHERE entry_id = ?1",
        "DELETE FROM entry_revisions WHERE entry_id = ?1",
    ] {
        let r = db
            .write(move |tx| {
                tx.execute(sql, [e.id])?;
                Ok(())
            })
            .await;
        let err = r.unwrap_err().to_string();
        assert!(err.contains("append-only"), "{sql}: {err}");
    }
    assert_eq!(entry_revisions(&db, e.id).await.unwrap().len(), 1);
}

#[tokio::test]
async fn supersede_marks_old_links_new_one_tx() {
    let (db, k, _) = setup("supersede").await;
    let (old, _) = create_entry(
        &db,
        insight(k, "Метод X нерабочий", "Не сработал."),
        &agent(),
    )
    .await
    .unwrap();
    let mut new = insight(
        k,
        "Метод X работает с параметром Y",
        "Параметры были неверны.",
    );
    new.idempotency_key = Some("sup-1".into());
    let me = agent();
    let (n, o) = supersede_entry(&db, old.id, None, new.clone(), &me)
        .await
        .unwrap();
    assert_eq!(n.supersedes_id, Some(old.id));
    assert_eq!(n.status, Some(EntryStatus::Active));
    assert_eq!(o.id, old.id);
    assert_eq!(o.status, Some(EntryStatus::Superseded));
    assert_eq!(o.superseded_by_id, Some(n.id));
    assert_eq!(o.rev, 2);
    // ничего не удалено, у старой — ревизия со статусом
    let revs = entry_revisions(&db, old.id).await.unwrap();
    assert_eq!(
        revs.iter().map(|r| r.status).collect::<Vec<_>>(),
        vec![Some(EntryStatus::Active), Some(EntryStatus::Superseded)]
    );
    // обе переиндексируются
    assert_eq!(
        jobs_of(&db, old.id).await,
        vec![("upsert".into(), "queued".into(), 2)]
    );
    assert_eq!(
        jobs_of(&db, n.id).await,
        vec![("upsert".into(), "queued".into(), 1)]
    );
    // фильтр статусов поиска: superseded по умолчанию не выдаётся
    let live = TargetFilter {
        statuses: vec!["active".into(), "revisit".into()],
        ..TargetFilter::default()
    };
    assert_eq!(fts(&db, "Метод", k, live).await, vec![n.id]);
    let mut all = fts(&db, "Метод", k, TargetFilter::default()).await;
    all.sort();
    let mut want = vec![old.id, n.id];
    want.sort();
    assert_eq!(all, want);
    // повтор с тем же ключом — та же пара
    let (n2, o2) = supersede_entry(&db, old.id, None, new.clone(), &me)
        .await
        .unwrap();
    assert_eq!((n2.id, o2.id), (n.id, old.id));
    // повторное замещение уже замещённой — конфликт, и новой записи не появилось
    let before: i64 = db
        .read(|c| Ok(c.query_row("SELECT count(*) FROM entries", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert!(matches!(
        supersede_entry(&db, old.id, None, insight(k, "третья", ""), &agent()).await,
        Err(StoreError::Conflict(_))
    ));
    // ошибка в середине транзакции: новая запись не остаётся (заголовок пустой)
    assert!(matches!(
        supersede_entry(&db, n.id, None, insight(k, " ", ""), &agent()).await,
        Err(StoreError::BadInput(_))
    ));
    let after: i64 = db
        .read(|c| Ok(c.query_row("SELECT count(*) FROM entries", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(before, after);
    assert_eq!(
        get_entry(&db, n.id).await.unwrap().status,
        Some(EntryStatus::Active)
    );
    // changelog не замещается
    let (c, _) = create_entry(&db, changelog(k, "ch"), &agent())
        .await
        .unwrap();
    assert!(matches!(
        supersede_entry(&db, c.id, None, insight(k, "x", ""), &agent()).await,
        Err(StoreError::BadInput(_))
    ));
}

#[tokio::test]
async fn retract_requires_reason() {
    let (db, k, _) = setup("retract").await;
    let (e, _) = create_entry(&db, insight(k, "Сомнительный вывод", "b"), &agent())
        .await
        .unwrap();
    for empty in ["", "   "] {
        assert!(matches!(
            retract_entry(&db, e.id, None, empty, &agent()).await,
            Err(StoreError::BadInput(_))
        ));
    }
    assert_eq!(get_entry(&db, e.id).await.unwrap().rev, 1);
    let r = retract_entry(&db, e.id, None, "опровергнуто экспериментом", &agent())
        .await
        .unwrap();
    assert_eq!(r.status, Some(EntryStatus::Retracted));
    assert_eq!(
        r.retract_reason.as_deref(),
        Some("опровергнуто экспериментом")
    );
    assert_eq!(r.rev, 2);
    let revs = entry_revisions(&db, e.id).await.unwrap();
    assert_eq!(
        revs[1].reason.as_deref(),
        Some("опровергнуто экспериментом")
    );
    assert_eq!(
        jobs_of(&db, e.id).await,
        vec![("upsert".into(), "queued".into(), 2)]
    );
    // повторный отзыв — конфликт; changelog не отзывается
    assert!(matches!(
        retract_entry(&db, e.id, None, "ещё раз", &agent()).await,
        Err(StoreError::Conflict(_))
    ));
    let (c, _) = create_entry(&db, changelog(k, "ch"), &agent())
        .await
        .unwrap();
    assert!(matches!(
        retract_entry(&db, c.id, None, "причина", &agent()).await,
        Err(StoreError::BadInput(_))
    ));
}

#[tokio::test]
async fn superseded_cannot_return_to_active() {
    let (db, k, _) = setup("no-return").await;
    let (old, _) = create_entry(&db, insight(k, "старая", "b"), &agent())
        .await
        .unwrap();
    supersede_entry(&db, old.id, None, insight(k, "новая", "b"), &agent())
        .await
        .unwrap();
    for s in [EntryStatus::Active, EntryStatus::Revisit] {
        let p = EntryPatch {
            status: Some(s),
            reason: Some("вернуть".into()),
            ..EntryPatch::default()
        };
        assert!(matches!(
            update_entry(&db, old.id, None, p, &agent()).await,
            Err(StoreError::Conflict(_))
        ));
    }
    assert_eq!(
        get_entry(&db, old.id).await.unwrap().status,
        Some(EntryStatus::Superseded)
    );
    // отозванная тоже не возвращается
    let (e, _) = create_entry(&db, insight(k, "x", "b"), &agent())
        .await
        .unwrap();
    retract_entry(&db, e.id, None, "неверно", &agent())
        .await
        .unwrap();
    let p = EntryPatch {
        status: Some(EntryStatus::Active),
        ..EntryPatch::default()
    };
    assert!(matches!(
        update_entry(&db, e.id, None, p, &agent()).await,
        Err(StoreError::Conflict(_))
    ));
    // active ↔ revisit — правкой
    let (e, _) = create_entry(&db, insight(k, "y", "b"), &agent())
        .await
        .unwrap();
    let p = EntryPatch {
        status: Some(EntryStatus::Revisit),
        ..EntryPatch::default()
    };
    let u = update_entry(&db, e.id, None, p, &agent()).await.unwrap();
    assert_eq!(u.status, Some(EntryStatus::Revisit));
    let p = EntryPatch {
        status: Some(EntryStatus::Active),
        ..EntryPatch::default()
    };
    let u = update_entry(&db, e.id, None, p, &agent()).await.unwrap();
    assert_eq!(u.status, Some(EntryStatus::Active));
}

#[tokio::test]
async fn deleted_entry_not_in_fts_restore_returns() {
    let (db, k, _) = setup("delete").await;
    let (e, _) = create_entry(
        &db,
        insight(k, "Уникальный ZQXW заголовок", "тело"),
        &agent(),
    )
    .await
    .unwrap();
    let jobs = claim_jobs(&db, 10, Timestamp::now()).await.unwrap();
    complete_job(&db, jobs[0].id, jobs[0].claim_gen)
        .await
        .unwrap();
    assert_eq!(
        fts(&db, "ZQXW", k, TargetFilter::default()).await,
        vec![e.id]
    );

    delete_entry(&db, e.id, None, &agent()).await.unwrap();
    assert!(
        fts(&db, "ZQXW", k, TargetFilter::default())
            .await
            .is_empty()
    );
    assert!(chunks_of(&db, e.id).await.is_empty());
    assert_eq!(
        jobs_of(&db, e.id).await,
        vec![("delete".into(), "queued".into(), 1)]
    );
    let jobs = claim_jobs(&db, 10, Timestamp::now()).await.unwrap();
    assert_eq!(jobs[0].op, JobOp::Delete);
    assert!(entry_chunks_for_index(&db, e.id).await.unwrap().is_empty());
    complete_job(&db, jobs[0].id, jobs[0].claim_gen)
        .await
        .unwrap();
    // удалённая запись: в ленте нет, по id — с deleted_at, ревизии на месте
    let got = get_entry(&db, e.id).await.unwrap();
    assert!(got.deleted_at.is_some());
    assert_eq!(entry_revisions(&db, e.id).await.unwrap().len(), 1);
    let feed = EntryFilter {
        space_id: k,
        kind: None,
        statuses: Vec::new(),
        certainty: Vec::new(),
        area: None,
        tag: None,
        data_kind: None,
        since: None,
        until: None,
        cursor: None,
        limit: 10,
    };
    assert!(list_entries(&db, feed.clone()).await.unwrap().0.is_empty());
    assert!(matches!(
        delete_entry(&db, e.id, None, &agent()).await,
        Err(StoreError::NotFound)
    ));
    assert!(matches!(
        update_entry(&db, e.id, None, EntryPatch::default(), &agent()).await,
        Err(StoreError::NotFound)
    ));

    let r = restore_entry(&db, e.id, None, &agent()).await.unwrap();
    assert!(r.deleted_at.is_none());
    assert_eq!(r.index_state, IndexState::Pending);
    assert_eq!(
        fts(&db, "ZQXW", k, TargetFilter::default()).await,
        vec![e.id]
    );
    assert_eq!(
        jobs_of(&db, e.id).await,
        vec![("upsert".into(), "queued".into(), 1)]
    );
    assert_eq!(list_entries(&db, feed).await.unwrap().0.len(), 1);
    assert!(matches!(
        restore_entry(&db, e.id, None, &agent()).await,
        Err(StoreError::BadInput(_))
    ));
}

#[tokio::test]
async fn list_entries_cursor_and_filters() {
    let (db, k, h) = setup("list").await;
    let mut ids = Vec::new();
    for i in 0..5 {
        let mut n = changelog(k, &format!("изменение {i}"));
        n.tags = if i % 2 == 0 {
            vec!["even".into()]
        } else {
            Vec::new()
        };
        n.area = (i < 2).then(|| "core".to_string());
        if i % 2 == 0 {
            n.data = json!({"kind": "feat"});
        }
        ids.push(create_entry(&db, n, &agent()).await.unwrap().0.id);
    }
    let (ins, _) = create_entry(&db, insight(k, "инсайт", "b"), &agent())
        .await
        .unwrap();
    let mut rev = insight(k, "пересмотреть", "b");
    rev.status = Some(EntryStatus::Revisit);
    rev.certainty = Some(Certainty::Theorem);
    // theorem от агента — только с доказательством (§7.5)
    rev.data["evidence"] = json!({"checked": "c", "method": "m", "result": "r"});
    let (rv, _) = create_entry(&db, rev, &agent()).await.unwrap();
    create_entry(&db, changelog(h, "чужое пространство"), &agent())
        .await
        .unwrap();
    let base = EntryFilter {
        space_id: k,
        kind: None,
        statuses: Vec::new(),
        certainty: Vec::new(),
        area: None,
        tag: None,
        data_kind: None,
        since: None,
        until: None,
        cursor: None,
        limit: 3,
    };
    // постранично, новые первыми, без повторов и пропусков
    let mut seen = Vec::new();
    let mut f = base.clone();
    loop {
        let (page, next) = list_entries(&db, f.clone()).await.unwrap();
        assert!(page.len() <= 3);
        seen.extend(page.iter().map(|e| e.id));
        match next {
            Some(c) => f.cursor = Some(c),
            None => break,
        }
    }
    let mut want: Vec<Id> = ids.clone();
    want.extend([ins.id, rv.id]);
    want.reverse();
    assert_eq!(seen, want);
    // вид
    let f = EntryFilter {
        kind: Some(EntryKind::Changelog),
        limit: 200,
        ..base.clone()
    };
    assert_eq!(list_entries(&db, f).await.unwrap().0.len(), 5);
    // статусы
    let f = EntryFilter {
        statuses: vec![EntryStatus::Revisit],
        limit: 200,
        ..base.clone()
    };
    let got: Vec<Id> = list_entries(&db, f)
        .await
        .unwrap()
        .0
        .iter()
        .map(|e| e.id)
        .collect();
    assert_eq!(got, vec![rv.id]);
    // certainty
    let f = EntryFilter {
        certainty: vec![Certainty::Hypothesis],
        limit: 200,
        ..base.clone()
    };
    let got: Vec<Id> = list_entries(&db, f)
        .await
        .unwrap()
        .0
        .iter()
        .map(|e| e.id)
        .collect();
    assert_eq!(got, vec![ins.id]);
    // область и тег
    let f = EntryFilter {
        area: Some("core".into()),
        limit: 200,
        ..base.clone()
    };
    assert_eq!(list_entries(&db, f).await.unwrap().0.len(), 2);
    let f = EntryFilter {
        tag: Some("even".into()),
        limit: 200,
        ..base.clone()
    };
    assert_eq!(list_entries(&db, f).await.unwrap().0.len(), 3);
    // data.kind: фильтр в SQL до LIMIT — курсор переходит границу страницы без потерь
    let mut f = EntryFilter {
        data_kind: Some("feat".into()),
        limit: 2,
        ..base.clone()
    };
    let (p1, next) = list_entries(&db, f.clone()).await.unwrap();
    assert_eq!(
        p1.iter().map(|e| e.id).collect::<Vec<_>>(),
        vec![ids[4], ids[2]]
    );
    f.cursor = Some(next.expect("вторая страница"));
    let (p2, next) = list_entries(&db, f).await.unwrap();
    assert_eq!(p2.iter().map(|e| e.id).collect::<Vec<_>>(), vec![ids[0]]);
    assert!(next.is_none());
    let f = EntryFilter {
        data_kind: Some("gotcha".into()),
        limit: 200,
        ..base.clone()
    };
    assert_eq!(list_entries(&db, f).await.unwrap().0.len(), 2);
    // интервал времени
    let all = list_entries(
        &db,
        EntryFilter {
            limit: 200,
            ..base.clone()
        },
    )
    .await
    .unwrap()
    .0;
    let pivot = all[2].created_at;
    let f = EntryFilter {
        since: Some(pivot),
        limit: 200,
        ..base.clone()
    };
    assert_eq!(list_entries(&db, f).await.unwrap().0.len(), 3);
    let f = EntryFilter {
        until: Some(pivot),
        limit: 200,
        ..base.clone()
    };
    assert_eq!(list_entries(&db, f).await.unwrap().0.len(), all.len() - 3);
    // пределы и курсор
    for limit in [0, 201] {
        let f = EntryFilter {
            limit,
            ..base.clone()
        };
        assert!(matches!(
            list_entries(&db, f).await,
            Err(StoreError::BadInput(_))
        ));
    }
    let f = EntryFilter {
        cursor: Some("мусор".into()),
        ..base.clone()
    };
    assert!(matches!(
        list_entries(&db, f).await,
        Err(StoreError::BadInput(_))
    ));
}

#[tokio::test]
async fn migration_v7_to_v8_keeps_data() {
    let dd = datadir("migv7");
    let user = Id::new();
    let path = dd.user_db(&user);
    let (space, node) = (Id::new(), Id::new());
    {
        let db = Db::open(&path, &USER_MIGRATIONS[..7]).unwrap();
        db.write(move |tx| {
            let now = "2026-10-20T00:00:00.000000Z";
            tx.execute(
                "INSERT INTO spaces (id, kind, name, created_at)
                 VALUES (?1, 'knowledge', 'Knowledge', ?2)",
                rusqlite::params![space, now],
            )?;
            tx.execute(
                "INSERT INTO nodes (id, space_id, kind, name, path, current_rev,
                                    index_state, created_at, updated_at)
                 VALUES (?1, ?2, 'note', 'n', 'n', 1, 'indexed', ?3, ?3)",
                rusqlite::params![node, space, now],
            )?;
            tx.execute(
                "INSERT INTO revisions (node_id, rev, content, content_hash, size,
                                        author_kind, message, ts, user_id)
                 VALUES (?1, 1, 'текст', 'h', 1, 'agent', 'm7', ?2, 'u7')",
                rusqlite::params![node, now],
            )?;
            tx.execute(
                "INSERT INTO index_jobs (target_id, rev, op, not_before, enqueued_at, updated_at)
                 VALUES (?1, 1, 'upsert', ?2, ?2, ?2)",
                rusqlite::params![node, now],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    }
    let dbs = UserDbs::new(dd.users_dir());
    let db = dbs.get(user).await.unwrap();
    // фоновые задачи открытия (рёбра v10, перенарезка с секретами) заново ставят задание узла
    // с not_before = своё «сейчас»: без ожидания claim_jobs с более ранним now его не видит
    dbs.wait_rechunk(user).await;
    assert!(path.with_file_name(format!("{user}.db.bak-v7")).exists());
    let ver: u32 = db
        .read(|c| Ok(c.query_row("PRAGMA user_version", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(ver, USER_MIGRATIONS.len() as u32);
    assert!(ver >= 8);
    // данные v7 на месте: узел, ревизия с автором, задание узла
    let (rev_user, job_kind): (String, String) = db
        .read(move |c| {
            Ok((
                c.query_row(
                    "SELECT user_id FROM revisions WHERE node_id = ?1",
                    [node],
                    |r| r.get(0),
                )?,
                c.query_row(
                    "SELECT target_kind FROM index_jobs WHERE target_id = ?1",
                    [node],
                    |r| r.get(0),
                )?,
            ))
        })
        .await
        .unwrap();
    assert_eq!((rev_user.as_str(), job_kind.as_str()), ("u7", "node"));
    let jobs = claim_jobs(&db, 10, Timestamp::now()).await.unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].target_kind, TargetKind::Node);
    assert_eq!(jobs[0].target_id, node);
    // новые таблицы работают
    let (e, _) = create_entry(&db, changelog(space, "после миграции"), &agent())
        .await
        .unwrap();
    assert_eq!(get_entry(&db, e.id).await.unwrap().title, "после миграции");
    // CHECK: статус только у insight — и на уровне схемы
    let bad = db
        .write(move |tx| {
            tx.execute("UPDATE entries SET status = 'active' WHERE id = ?1", [e.id])?;
            Ok(())
        })
        .await;
    assert!(bad.is_err());
}

/// Число строк таблицы `table`.
async fn count(db: &Db, table: &'static str) -> i64 {
    db.read(move |c| Ok(c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?))
        .await
        .unwrap()
}

fn trig(kind: TriggerKind, pattern: &str) -> NewTrigger {
    NewTrigger {
        kind,
        pattern: pattern.into(),
        tools: None,
    }
}

/// Счётчики таблиц, которые пишет создание или замещение записи.
async fn footprint(db: &Db) -> [i64; 5] {
    [
        count(db, "entries").await,
        count(db, "entry_revisions").await,
        count(db, "chunks").await,
        count(db, "index_jobs").await,
        count(db, "triggers").await,
    ]
}

#[tokio::test]
async fn supersede_rolls_back_after_partial_write() {
    let (db, k, _) = setup("supersede-rollback").await;
    let (old, _) = create_entry(&db, insight(k, "Старый вывод", "Текст."), &agent())
        .await
        .unwrap();
    let before = footprint(&db).await;
    // сбой уже после вставки новой записи (ревизия, чанки, задание, триггер) и
    // `superseded_by_id` у старой: смену статуса старой отклоняет триггер SQLite
    db.write(|tx| {
        tx.execute_batch(
            "CREATE TRIGGER fail_supersede BEFORE UPDATE OF status ON entries
             WHEN NEW.status = 'superseded' BEGIN SELECT RAISE(ABORT, 'сбой теста'); END;",
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let mut new = insight(k, "Новый вывод", "Исправленный текст.");
    new.triggers = vec![trig(TriggerKind::Keyword, "вывод")];
    assert!(
        supersede_entry(&db, old.id, None, new.clone(), &agent())
            .await
            .is_err()
    );
    // ничего не записано: старая как была, новой нет
    assert_eq!(get_entry(&db, old.id).await.unwrap(), old);
    assert_eq!(entry_revisions(&db, old.id).await.unwrap().len(), 1);
    assert_eq!(footprint(&db).await, before);
    // без сбоя замещение проходит
    db.write(|tx| {
        tx.execute_batch("DROP TRIGGER fail_supersede")?;
        Ok(())
    })
    .await
    .unwrap();
    let (n, o) = supersede_entry(&db, old.id, None, new, &agent())
        .await
        .unwrap();
    assert_eq!(o.status, Some(EntryStatus::Superseded));
    assert_eq!(entry_triggers(&db, n.id).await.unwrap().len(), 1);
}

#[tokio::test]
async fn triggers_stored_replaced_and_active_only() {
    let (db, k, _) = setup("triggers").await;
    let mut e = insight(k, "LF в скриптах", "Текст.");
    e.triggers = vec![
        NewTrigger {
            tools: Some(vec![" Write ".into(), "Edit".into()]),
            ..trig(TriggerKind::FileGlob, "**/*.sh")
        },
        trig(TriggerKind::ErrorRegex, r"\^M|bad interpreter"),
    ];
    let (a, _) = create_entry(&db, e, &agent()).await.unwrap();
    let ts = entry_triggers(&db, a.id).await.unwrap();
    assert_eq!(
        ts.iter()
            .map(|t| (t.kind, t.pattern.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (TriggerKind::FileGlob, "**/*.sh"),
            (TriggerKind::ErrorRegex, r"\^M|bad interpreter")
        ]
    );
    assert_eq!(
        ts[0].tools,
        Some(vec!["Write".to_string(), "Edit".to_string()])
    );
    let act = active_triggers(&db, &[k]).await.unwrap();
    assert_eq!(act.len(), 2);
    assert_eq!(act[0].entry_id, a.id);
    assert_eq!(act[0].title, "LF в скриптах");
    assert_eq!(act[0].insight_kind, "gotcha");
    assert_eq!(act[0].certainty, Some(Certainty::Hypothesis));
    assert_eq!(act[0].space_id, k);
    // неверный триггер — BadInput с индексом, набор не меняется
    let bad = set_triggers(
        &db,
        a.id,
        vec![
            trig(TriggerKind::Keyword, "ok"),
            trig(TriggerKind::CommandRegex, "("),
        ],
        AuthorKind::Agent,
    )
    .await;
    assert!(
        matches!(&bad, Err(StoreError::BadInput(m)) if m.contains("triggers[1]")),
        "{bad:?}"
    );
    assert_eq!(entry_triggers(&db, a.id).await.unwrap().len(), 2);
    // замена набора целиком
    set_triggers(
        &db,
        a.id,
        vec![trig(TriggerKind::Keyword, "crlf")],
        AuthorKind::Agent,
    )
    .await
    .unwrap();
    let ts = entry_triggers(&db, a.id).await.unwrap();
    assert_eq!(ts.len(), 1);
    assert_eq!(ts[0].pattern, "crlf");
    // правкой — тоже, в той же транзакции; ревизии триггеры не создают
    let p = EntryPatch {
        triggers: Some(vec![trig(TriggerKind::Keyword, "lf")]),
        ..EntryPatch::default()
    };
    let same = update_entry(&db, a.id, None, p, &agent()).await.unwrap();
    assert_eq!(same.rev, a.rev);
    assert_eq!(entry_triggers(&db, a.id).await.unwrap()[0].pattern, "lf");
    // revisit остаётся в выдаче, superseded, retracted и удалённые — нет
    let with = |title: &str, kw: &str, status: Option<EntryStatus>| NewEntry {
        status,
        triggers: vec![trig(TriggerKind::Keyword, kw)],
        ..insight(k, title, "")
    };
    let (b, _) = create_entry(&db, with("Б", "b", Some(EntryStatus::Revisit)), &agent())
        .await
        .unwrap();
    let (c, _) = create_entry(&db, with("В", "c", None), &agent())
        .await
        .unwrap();
    let (d, _) = create_entry(&db, with("Г", "d", None), &agent())
        .await
        .unwrap();
    let ids = |v: Vec<zam_store::TriggerRow>| {
        let mut v: Vec<Id> = v.into_iter().map(|t| t.entry_id).collect();
        v.sort();
        v.dedup();
        v
    };
    let mut want = vec![a.id, b.id, c.id, d.id];
    want.sort();
    assert_eq!(ids(active_triggers(&db, &[k]).await.unwrap()), want);
    retract_entry(&db, c.id, None, "неверно", &agent())
        .await
        .unwrap();
    delete_entry(&db, d.id, None, &agent()).await.unwrap();
    supersede_entry(&db, a.id, None, insight(k, "Новая", ""), &agent())
        .await
        .unwrap();
    assert_eq!(ids(active_triggers(&db, &[k]).await.unwrap()), vec![b.id]);
    // чужое пространство и пустой список — пусто
    assert!(active_triggers(&db, &[Id::new()]).await.unwrap().is_empty());
    assert!(active_triggers(&db, &[]).await.unwrap().is_empty());
    // триггеры только у insight
    let mut ch = changelog(k, "ch");
    ch.triggers = vec![trig(TriggerKind::Keyword, "x")];
    assert!(matches!(
        create_entry(&db, ch, &agent()).await,
        Err(StoreError::BadInput(_))
    ));
    let (ch, _) = create_entry(&db, changelog(k, "ch"), &agent())
        .await
        .unwrap();
    assert!(matches!(
        set_triggers(&db, ch.id, vec![], AuthorKind::Agent).await,
        Err(StoreError::BadInput(_))
    ));
}

#[tokio::test]
async fn title_duplicates_same_area_and_active_only() {
    let (db, k, _) = setup("title-dups").await;
    let mut web = agent();
    web.area = Some("web".into());
    let (a, _) = create_entry(&db, insight(k, "Bash-скрипты: LF!", "x"), &web)
        .await
        .unwrap();
    create_entry(&db, insight(k, "bash скрипты lf", "без области"), &agent())
        .await
        .unwrap();
    let got = title_duplicates(&db, k, "  BASH скрипты — LF ", Some(" web "))
        .await
        .unwrap();
    assert_eq!(got.iter().map(|c| c.id).collect::<Vec<_>>(), vec![a.id]);
    assert_eq!((got[0].reason, got[0].score), ("title", None));
    assert_eq!(got[0].status, EntryStatus::Active);
    // без области — только запись без области; другая область — ничего
    let dups = |area: Option<&'static str>| {
        let db = db.clone();
        async move {
            title_duplicates(&db, k, "bash скрипты lf", area)
                .await
                .unwrap()
                .len()
        }
    };
    assert_eq!(dups(None).await, 1);
    assert_eq!(dups(Some("api")).await, 0);
    // отозванная — не дубль; заголовок без букв и цифр дублей не имеет
    retract_entry(&db, a.id, None, "ошибка", &agent())
        .await
        .unwrap();
    assert_eq!(dups(Some("web")).await, 0);
    assert!(
        title_duplicates(&db, k, "—", None)
            .await
            .unwrap()
            .is_empty()
    );
}
