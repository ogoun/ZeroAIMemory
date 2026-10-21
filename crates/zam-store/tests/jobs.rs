//! Outbox индексации (§8.1 ТЗ): задания `index_jobs` при записи, очередь с backoff,
//! чанки уровней §8.2 для воркера, миграция 0004 и разовая перечанковка (Ruling R1).

use std::path::PathBuf;

use jiff::{SignedDuration, Timestamp};
use zam_core::Id;
use zam_store::{
    Attribution, AuthorKind, DataDir, Db, IndexState, JobOp, JobOutcome, NewNote, NodePath,
    SpaceKind, USER_MIGRATIONS, UserDbs, chunks_for_index, claim_jobs, complete_job, create_note,
    delete_node, enqueue_reindex, fail_job, get_node, indexed_count, job_stats, list_spaces,
    move_node, release_job, requeue_running, restore_node, set_index_state, update_note,
};

fn datadir(name: &str) -> DataDir {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("jobs-{name}"));
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

fn attrib() -> Attribution {
    Attribution::new(AuthorKind::Agent)
}

async fn add(db: &Db, space: Id, path: &str, content: &str) -> Id {
    create_note(
        db,
        NewNote {
            space_id: space,
            path: NodePath::parse(path).unwrap(),
            content: content.into(),
            l0: None,
            attrib: attrib(),
        },
    )
    .await
    .unwrap()
    .id
}

/// `(op, state, rev)` всех заданий узла по порядку id.
async fn jobs_of(db: &Db, node: Id) -> Vec<(String, String, i64)> {
    db.read(move |c| {
        let mut st = c.prepare(
            "SELECT op, state, rev FROM index_jobs
             WHERE target_kind = 'node' AND target_id = ?1 ORDER BY id",
        )?;
        let rows = st.query_map([node], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    })
    .await
    .unwrap()
}

/// `(level, token_count)` чанков узла по порядку.
async fn levels_of(db: &Db, node: Id) -> Vec<(String, i64)> {
    db.read(move |c| {
        let mut st =
            c.prepare("SELECT level, token_count FROM chunks WHERE node_id = ?1 ORDER BY ordinal")?;
        let rows = st.query_map([node], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    })
    .await
    .unwrap()
}

const NOTE: &str = "# Сборка\n\nСобираем проект командой cargo build. Тесты гоняем отдельно.\n\n## Детали\n\nПеременные окружения задаются скриптом dev-env.\n";

#[tokio::test]
async fn write_enqueues_upsert_with_chunk_and_note_levels() {
    let (db, k, _) = setup("write").await;
    let id = add(&db, k, "a/b", NOTE).await;
    assert_eq!(
        jobs_of(&db, id).await,
        vec![("upsert".into(), "queued".into(), 1)]
    );
    let levels = levels_of(&db, id).await;
    assert!(levels.iter().any(|(l, _)| l == "chunk"), "{levels:?}");
    assert!(levels.iter().any(|(l, _)| l == "note"), "{levels:?}");
    assert!(levels.iter().all(|(_, t)| *t > 0), "{levels:?}");
    assert_eq!(get_node(&db, id).await.unwrap().index_state, "pending");

    // повторная запись заменяет активное задание, а не плодит второе
    update_note(&db, id, 1, format!("{NOTE}\nЕщё абзац.\n"), None, attrib())
        .await
        .unwrap();
    assert_eq!(
        jobs_of(&db, id).await,
        vec![("upsert".into(), "queued".into(), 2)]
    );
    let st = job_stats(&db).await.unwrap();
    assert_eq!((st.queued, st.running, st.failed), (1, 0, 0));
    assert!(st.oldest.is_some());
}

#[tokio::test]
async fn delete_enqueues_delete_and_restore_upsert() {
    let (db, k, _) = setup("delete").await;
    let id = add(&db, k, "dir/x", NOTE).await;
    let now = Timestamp::now();
    let claimed = claim_jobs(&db, 10, now).await.unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(
        complete_job(&db, claimed[0].id, claimed[0].claim_gen)
            .await
            .unwrap(),
        JobOutcome::Done
    );
    assert!(
        jobs_of(&db, id).await.is_empty(),
        "выполненное задание удалено"
    );
    set_index_state(&db, id, 1, IndexState::Indexed)
        .await
        .unwrap();
    assert_eq!(get_node(&db, id).await.unwrap().index_state, "indexed");

    // удаление папки ставит delete для заметки внутри
    let dir = zam_store::get_node_by_path(&db, k, &NodePath::parse("dir").unwrap())
        .await
        .unwrap();
    delete_node(&db, dir.id).await.unwrap();
    assert_eq!(
        jobs_of(&db, id).await,
        vec![("delete".into(), "queued".into(), 1)]
    );
    assert!(chunks_for_index(&db, id).await.unwrap().is_empty());

    restore_node(&db, dir.id).await.unwrap();
    let jobs = jobs_of(&db, id).await;
    assert_eq!(
        jobs,
        vec![("upsert".into(), "queued".into(), 1)],
        "delete заменён на upsert"
    );
    assert_eq!(get_node(&db, id).await.unwrap().index_state, "pending");
}

#[tokio::test]
async fn human_and_index_false_get_no_jobs() {
    let (db, k, h) = setup("excluded").await;
    let a = add(&db, h, "личное", NOTE).await;
    let b = add(&db, k, "скрыто", &format!("---\nindex: false\n---\n{NOTE}")).await;
    for id in [a, b] {
        assert!(jobs_of(&db, id).await.is_empty());
        assert_eq!(get_node(&db, id).await.unwrap().index_state, "excluded");
        assert!(chunks_for_index(&db, id).await.unwrap().is_empty());
        // FTS по-прежнему работает: чанки есть
        assert!(!levels_of(&db, id).await.is_empty());
    }
    delete_node(&db, a).await.unwrap();
    assert!(jobs_of(&db, a).await.is_empty());

    // индексированная заметка, ставшая index: false, удаляется из индекса
    let c = add(&db, k, "станет-скрытой", NOTE).await;
    update_note(
        &db,
        c,
        1,
        format!("---\nindex: false\n---\n{NOTE}"),
        None,
        attrib(),
    )
    .await
    .unwrap();
    assert_eq!(
        jobs_of(&db, c).await,
        vec![("delete".into(), "queued".into(), 2)]
    );
}

#[tokio::test]
async fn backoff_then_failed() {
    let (db, k, _) = setup("backoff").await;
    let id = add(&db, k, "n", NOTE).await;
    // время в БД — с точностью до микросекунд
    let mut now = Timestamp::from_microsecond(Timestamp::now().as_microsecond()).unwrap();
    let mut prev_delay = 0i64;
    for attempt in 1..=5i64 {
        let jobs = claim_jobs(&db, 10, now).await.unwrap();
        assert_eq!(jobs.len(), 1, "попытка {attempt}");
        assert_eq!(jobs[0].attempts, attempt - 1);
        assert_eq!(jobs[0].op, JobOp::Upsert);
        assert_eq!(jobs[0].target_id, id);
        // занятое задание повторно не выдаётся
        assert!(claim_jobs(&db, 10, now).await.unwrap().is_empty());
        let out = fail_job(&db, jobs[0].id, jobs[0].claim_gen, "qdrant недоступен", now)
            .await
            .unwrap();
        // повтор той же выдачи устарел
        assert_eq!(
            fail_job(&db, jobs[0].id, jobs[0].claim_gen, "x", now)
                .await
                .unwrap(),
            JobOutcome::Stale
        );
        if attempt == 5 {
            assert_eq!(out, JobOutcome::Failed);
            break;
        }
        assert_eq!(out, JobOutcome::Retry);
        let nb: String = db
            .read(move |c| Ok(c.query_row("SELECT not_before FROM index_jobs", [], |r| r.get(0))?))
            .await
            .unwrap();
        let nb: Timestamp = nb.parse().unwrap();
        let delay = nb.duration_since(now).as_secs();
        assert_eq!(delay, 1 << attempt, "попытка {attempt}");
        assert!(delay > prev_delay);
        prev_delay = delay;
        // до not_before задание не выдаётся
        assert!(
            claim_jobs(&db, 10, now + SignedDuration::from_secs(delay - 1))
                .await
                .unwrap()
                .is_empty()
        );
        now += SignedDuration::from_secs(delay);
    }
    assert_eq!(
        jobs_of(&db, id).await,
        vec![("upsert".into(), "failed".into(), 1)]
    );
    let st = job_stats(&db).await.unwrap();
    assert_eq!((st.queued, st.running, st.failed), (0, 0, 1));
    assert_eq!(get_node(&db, id).await.unwrap().index_state, "failed");
    assert!(
        claim_jobs(&db, 10, now + SignedDuration::from_hours(2))
            .await
            .unwrap()
            .is_empty()
    );
    // новая запись ставит новое задание рядом с проваленным
    update_note(&db, id, 1, format!("{NOTE}\nновое\n"), None, attrib())
        .await
        .unwrap();
    assert_eq!(jobs_of(&db, id).await.len(), 2);
}

#[tokio::test]
async fn claim_order_requeue_and_superseded_running() {
    let (db, k, _) = setup("claim").await;
    let a = add(&db, k, "a", NOTE).await;
    let b = add(&db, k, "b", NOTE).await;
    let now = Timestamp::now();
    let first = claim_jobs(&db, 1, now).await.unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].target_id, a, "приоритет равный — по id");

    // «падение»: задание осталось running; при старте возвращается в очередь
    assert_eq!(requeue_running(&db).await.unwrap(), 1);
    let again = claim_jobs(&db, 10, now).await.unwrap();
    assert_eq!(
        again.iter().map(|j| j.target_id).collect::<Vec<_>>(),
        vec![a, b]
    );

    // b обработано чисто — строка удалена
    assert_eq!(
        complete_job(&db, again[1].id, again[1].claim_gen)
            .await
            .unwrap(),
        JobOutcome::Done
    );
    assert!(jobs_of(&db, b).await.is_empty());
    // выдача до «падения» устарела: её завершение не принимается
    assert_eq!(
        complete_job(&db, first[0].id, first[0].claim_gen)
            .await
            .unwrap(),
        JobOutcome::Stale
    );
    assert_eq!(
        jobs_of(&db, a).await,
        vec![("upsert".into(), "running".into(), 1)]
    );
    assert!(again[0].claim_gen > first[0].claim_gen);
}

/// Запись во время обработки: задание остаётся `running` (второй раз не выдаётся),
/// получает новую ревизию и после завершения устаревшей обработки снова в очереди.
#[tokio::test]
async fn write_during_processing_requeues_after_completion() {
    let (db, k, _) = setup("dirty").await;
    let a = add(&db, k, "a", NOTE).await;
    let now = Timestamp::now();
    let stale = claim_jobs(&db, 10, now).await.unwrap().remove(0);
    update_note(&db, a, 1, format!("{NOTE}\nправка\n"), None, attrib())
        .await
        .unwrap();
    assert_eq!(
        jobs_of(&db, a).await,
        vec![("upsert".into(), "running".into(), 2)]
    );
    // пока обработка идёт, узел повторно не выдаётся
    assert!(claim_jobs(&db, 10, now).await.unwrap().is_empty());
    // устаревшая ревизия не меняет index_state
    set_index_state(&db, a, 1, IndexState::Indexed)
        .await
        .unwrap();
    assert_eq!(get_node(&db, a).await.unwrap().index_state, "pending");
    assert_eq!(
        complete_job(&db, stale.id, stale.claim_gen).await.unwrap(),
        JobOutcome::Requeued
    );
    assert_eq!(
        jobs_of(&db, a).await,
        vec![("upsert".into(), "queued".into(), 2)]
    );
    // новая ревизия обрабатывается следующей выдачей; старая выдача — Stale
    let fresh = claim_jobs(&db, 10, Timestamp::now())
        .await
        .unwrap()
        .remove(0);
    assert_eq!((fresh.id, fresh.rev, fresh.attempts), (stale.id, 2, 0));
    assert_eq!(
        complete_job(&db, stale.id, stale.claim_gen).await.unwrap(),
        JobOutcome::Stale
    );
    assert_eq!(
        jobs_of(&db, a).await,
        vec![("upsert".into(), "running".into(), 2)]
    );
    assert_eq!(
        complete_job(&db, fresh.id, fresh.claim_gen).await.unwrap(),
        JobOutcome::Done
    );
    assert!(jobs_of(&db, a).await.is_empty());
}

/// Неудача «грязного» задания: в очередь сразу, попытки с нуля (новая ревизия
/// ещё не пробовалась); `requeue_running` снимает `dirty`.
#[tokio::test]
async fn fail_on_dirty_requeues_with_zero_attempts() {
    let (db, k, _) = setup("dirty-fail").await;
    let a = add(&db, k, "a", NOTE).await;
    let now = Timestamp::from_microsecond(Timestamp::now().as_microsecond()).unwrap();
    let j = claim_jobs(&db, 10, now).await.unwrap().remove(0);
    assert_eq!(
        fail_job(&db, j.id, j.claim_gen, "сбой", now).await.unwrap(),
        JobOutcome::Retry
    );
    let j = claim_jobs(&db, 10, now + SignedDuration::from_secs(2))
        .await
        .unwrap()
        .remove(0);
    assert_eq!(j.attempts, 1);
    zam_store::patch_note(
        &db,
        a,
        1,
        vec![zam_markdown::PatchOp::Append {
            content: "хвост\n".into(),
        }],
        attrib(),
    )
    .await
    .unwrap();
    assert_eq!(
        fail_job(&db, j.id, j.claim_gen, "сбой", now).await.unwrap(),
        JobOutcome::Requeued
    );
    let j = claim_jobs(&db, 10, now).await.unwrap().remove(0);
    assert_eq!((j.rev, j.attempts), (2, 0));

    // «падение» с грязным заданием: после requeue_running оно чистое
    zam_store::revert_note(&db, a, 1, attrib()).await.unwrap();
    assert_eq!(requeue_running(&db).await.unwrap(), 1);
    let j = claim_jobs(&db, 10, now).await.unwrap().remove(0);
    assert_eq!(j.rev, 3, "откат ставит upsert");
    assert_eq!(
        complete_job(&db, j.id, j.claim_gen).await.unwrap(),
        JobOutcome::Done
    );
}

/// Перенос во время обработки (ревизия та же): задание снова в очереди.
#[tokio::test]
async fn move_replaces_running_job() {
    let (db, k, _) = setup("move-running").await;
    let a = add(&db, k, "x/a", NOTE).await;
    let j = claim_jobs(&db, 10, Timestamp::now())
        .await
        .unwrap()
        .remove(0);
    move_node(&db, a, NodePath::parse("y/a").unwrap())
        .await
        .unwrap();
    assert_eq!(
        complete_job(&db, j.id, j.claim_gen).await.unwrap(),
        JobOutcome::Requeued
    );
    assert_eq!(
        jobs_of(&db, a).await,
        vec![("upsert".into(), "queued".into(), 1)]
    );
    let chunks = chunks_for_index(&db, a).await.unwrap();
    assert_eq!(chunks[0].node.path, "y/a");
}

/// `indexed` ставит сам `complete_job` (в той же транзакции, только `Done` upsert и без
/// активного задания): перенос во время обработки оставляет узел `pending`, а не `indexed`.
#[tokio::test]
async fn complete_marks_indexed_atomically() {
    let (db, k, _) = setup("complete-indexed").await;
    let a = add(&db, k, "x/a", NOTE).await;
    let state = async |db: &Db| get_node(db, a).await.unwrap().index_state;
    let j = claim_jobs(&db, 10, Timestamp::now())
        .await
        .unwrap()
        .remove(0);
    move_node(&db, a, NodePath::parse("y/a").unwrap())
        .await
        .unwrap();
    assert_eq!(
        complete_job(&db, j.id, j.claim_gen).await.unwrap(),
        JobOutcome::Requeued
    );
    assert_eq!(state(&db).await, "pending");
    let j = claim_jobs(&db, 10, Timestamp::now())
        .await
        .unwrap()
        .remove(0);
    assert_eq!(
        complete_job(&db, j.id, j.claim_gen).await.unwrap(),
        JobOutcome::Done
    );
    assert_eq!(
        state(&db).await,
        "indexed",
        "без отдельного set_index_state"
    );
    // устаревшая выдача ничего не меняет
    assert_eq!(
        complete_job(&db, j.id, j.claim_gen).await.unwrap(),
        JobOutcome::Stale
    );
    // delete не трогает состояние
    delete_node(&db, a).await.unwrap();
    restore_node(&db, a).await.unwrap();
    assert_eq!(state(&db).await, "pending");
}

/// Восстановление заметки, ставшей `index: false` до удаления: заданий нет.
#[tokio::test]
async fn restore_of_excluded_note_has_no_job() {
    let (db, k, _) = setup("restore-excluded").await;
    let a = add(&db, k, "a", NOTE).await;
    let j = claim_jobs(&db, 10, Timestamp::now())
        .await
        .unwrap()
        .remove(0);
    complete_job(&db, j.id, j.claim_gen).await.unwrap();
    update_note(
        &db,
        a,
        1,
        format!("---\nindex: false\n---\n{NOTE}"),
        None,
        attrib(),
    )
    .await
    .unwrap();
    let j = claim_jobs(&db, 10, Timestamp::now())
        .await
        .unwrap()
        .remove(0);
    assert_eq!(j.op, JobOp::Delete);
    complete_job(&db, j.id, j.claim_gen).await.unwrap();
    delete_node(&db, a).await.unwrap();
    assert!(
        jobs_of(&db, a).await.is_empty(),
        "excluded: delete не нужен"
    );
    let n = restore_node(&db, a).await.unwrap();
    assert_eq!(n.index_state, "excluded");
    assert!(jobs_of(&db, a).await.is_empty());
    assert!(chunks_for_index(&db, a).await.unwrap().is_empty());
}

/// Заметка без текста тела: один M1-чанк заголовка уровня `chunk` (L1 заметки
/// чанкер не строит — предложений нет), он индексируется и находится FTS.
#[tokio::test]
async fn title_only_note_chunk_is_indexable() {
    let (db, k, _) = setup("title").await;
    let a = add(&db, k, "t", "# Заголовокзаметки\n").await;
    assert_eq!(levels_of(&db, a).await.len(), 1);
    assert_eq!(levels_of(&db, a).await[0].0, "chunk");
    let chunks = chunks_for_index(&db, a).await.unwrap();
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].text, "Заголовокзаметки");
    assert_eq!(
        jobs_of(&db, a).await,
        vec![("upsert".into(), "queued".into(), 1)]
    );
    // текст появился — заголовочный чанк заменён листом и L1 заметки
    update_note(
        &db,
        a,
        1,
        "# Заголовокзаметки\n\nПервое предложение тела. Второе предложение.\n".into(),
        None,
        attrib(),
    )
    .await
    .unwrap();
    let levels: Vec<String> = levels_of(&db, a).await.into_iter().map(|l| l.0).collect();
    assert_eq!(levels, vec!["chunk", "note"]);
}

#[tokio::test]
async fn chunks_for_index_payload() {
    let (db, k, _) = setup("payload").await;
    let src = format!("---\ntags: [rust, сборка]\n---\n{NOTE}");
    let id = add(&db, k, "Инструменты/Rust/Сборка", &src).await;
    let chunks = chunks_for_index(&db, id).await.unwrap();
    assert!(!chunks.is_empty());
    let c = &chunks[0];
    assert_eq!(c.level, zam_markdown::ChunkLevel::Chunk);
    assert_eq!(c.rev, 1);
    assert_eq!(c.id.len(), 32);
    assert_eq!(c.text_hash.len(), 64);
    assert!(c.token_count > 0);
    assert!(
        c.context_prefix
            .starts_with("[Knowledge/Инструменты/Rust/Сборка]")
    );
    let p = &c.node;
    assert_eq!(p.node_id, id);
    assert_eq!(p.space_id, k);
    assert_eq!(p.space_kind, SpaceKind::Knowledge);
    assert_eq!(p.solution_guid, None);
    assert_eq!(p.path, "Инструменты/Rust/Сборка");
    assert_eq!(
        p.path_ancestors,
        vec![
            "Инструменты".to_string(),
            "Инструменты/Rust".into(),
            "Инструменты/Rust/Сборка".into()
        ]
    );
    assert_eq!(p.tags, vec!["rust".to_string(), "сборка".into()]);
    assert!(
        chunks
            .iter()
            .any(|c| c.level == zam_markdown::ChunkLevel::Note)
    );

    // перенос ставит upsert (меняются путь и context_prefix)
    let now = Timestamp::now();
    for j in claim_jobs(&db, 10, now).await.unwrap() {
        complete_job(&db, j.id, j.claim_gen).await.unwrap();
    }
    move_node(&db, id, NodePath::parse("Rust/Сборка").unwrap())
        .await
        .unwrap();
    let jobs = jobs_of(&db, id).await;
    assert_eq!(jobs.last().unwrap(), &("upsert".into(), "queued".into(), 1));
    let chunks = chunks_for_index(&db, id).await.unwrap();
    assert_eq!(chunks[0].node.path_ancestors, vec!["Rust", "Rust/Сборка"]);
    assert!(
        chunks[0]
            .context_prefix
            .starts_with("[Knowledge/Rust/Сборка]")
    );
}

#[tokio::test]
async fn reindex_enqueues_indexable_notes() {
    let (db, k, h) = setup("reindex").await;
    let a = add(&db, k, "a", NOTE).await;
    add(&db, k, "b", NOTE).await;
    add(&db, h, "c", NOTE).await;
    add(&db, k, "d", &format!("---\nindex: false\n---\n{NOTE}")).await;
    for j in claim_jobs(&db, 10, Timestamp::now()).await.unwrap() {
        complete_job(&db, j.id, j.claim_gen).await.unwrap();
    }
    assert_eq!(enqueue_reindex(&db, None).await.unwrap(), 2);
    assert_eq!(job_stats(&db).await.unwrap().queued, 2);
    // повторный reindex не плодит активных заданий
    assert_eq!(enqueue_reindex(&db, Some(k)).await.unwrap(), 2);
    assert_eq!(job_stats(&db).await.unwrap().queued, 2);
    assert_eq!(enqueue_reindex(&db, Some(h)).await.unwrap(), 0);
    assert_eq!(jobs_of(&db, a).await.len(), 1);
}

/// v3 (M1): чанки `section`, Human — `pending`. Открытие v4 сохраняет данные,
/// перечанковывает все живые заметки и ставит `upsert` (один раз).
#[tokio::test]
async fn migrate_from_v3_rechunks_once() {
    let dd = datadir("migv3");
    let user = Id::new();
    let path = dd.user_db(&user);
    let (k, h) = (Id::new(), Id::new());
    let (n1, n2, gone) = (Id::new(), Id::new(), Id::new());
    {
        let db = Db::open(&path, &USER_MIGRATIONS[..3]).unwrap();
        db.write(move |tx| {
            let now = "2026-10-18T00:00:00.000000Z";
            for (id, kind, name) in [(k, "knowledge", "Knowledge"), (h, "human", "Human")] {
                tx.execute(
                    "INSERT INTO spaces (id, kind, name, created_at) VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![id, kind, name, now],
                )?;
            }
            for (id, space, p, deleted) in [
                (n1, k, "заметка", None),
                (n2, h, "дневник", None),
                (gone, k, "удалена", Some(now)),
            ] {
                tx.execute(
                    "INSERT INTO nodes (id, space_id, kind, name, path, current_rev,
                                        index_state, created_at, updated_at, deleted_at)
                     VALUES (?1, ?2, 'note', ?3, ?3, 1, 'pending', ?4, ?4, ?5)",
                    rusqlite::params![id, space, p, now, deleted],
                )?;
                tx.execute(
                    "INSERT INTO revisions (node_id, rev, content, content_hash, size,
                                            author_kind, ts)
                     VALUES (?1, 1, ?2, 'h', 1, 'agent', ?3)",
                    rusqlite::params![id, NOTE, now],
                )?;
                if deleted.is_none() {
                    tx.execute(
                        "INSERT INTO chunks (id, node_id, rev, level, ordinal, text,
                                             context_prefix, token_count, text_hash)
                         VALUES (?1, ?2, 1, 'section', 0, 'старый чанк уникальнословоэм',
                                 '', 3, 'x')",
                        rusqlite::params![format!("{id}"), id],
                    )?;
                }
            }
            Ok(())
        })
        .await
        .unwrap();
    }
    let dbs = UserDbs::new(dd.users_dir());
    let db = dbs.get(user).await.unwrap();
    // перечанковка идёт в фоне: get её не ждёт
    dbs.wait_rechunk(user).await;
    assert!(path.with_file_name(format!("{user}.db.bak-v3")).exists());
    let ver: u32 = db
        .read(|c| Ok(c.query_row("PRAGMA user_version", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(ver, USER_MIGRATIONS.len() as u32);
    // старые чанки ушли из FTS вместе с таблицей
    let stale: i64 = db
        .read(|c| {
            Ok(c.query_row(
                "SELECT count(*) FROM chunks_fts WHERE chunks_fts MATCH 'уникальнословоэм'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(stale, 0);
    let levels = levels_of(&db, n1).await;
    assert!(levels.iter().any(|(l, _)| l == "chunk"), "{levels:?}");
    assert!(levels.iter().any(|(l, _)| l == "note"), "{levels:?}");
    assert_eq!(
        jobs_of(&db, n1).await,
        vec![("upsert".into(), "queued".into(), 1)]
    );
    assert!(jobs_of(&db, n2).await.is_empty(), "Human без заданий");
    assert_eq!(get_node(&db, n2).await.unwrap().index_state, "excluded");
    assert!(jobs_of(&db, gone).await.is_empty());
    let found = zam_store::search_fts(
        &db,
        zam_store::FtsQuery {
            text: "cargo build".into(),
            space_ids: vec![k],
            path_prefix: None,
            top_k: 10,
        },
    )
    .await
    .unwrap();
    assert!(found.iter().any(|f| f.node_id == n1));
    drop((db, dbs));

    // повторное открытие перечанковку не повторяет
    let db = UserDbs::new(dd.users_dir()).get(user).await.unwrap();
    for j in claim_jobs(&db, 10, Timestamp::now()).await.unwrap() {
        complete_job(&db, j.id, j.claim_gen).await.unwrap();
    }
    drop(db);
    let db = UserDbs::new(dd.users_dir()).get(user).await.unwrap();
    assert_eq!(job_stats(&db).await.unwrap().queued, 0);
}

/// Задержка «запись → FTS» на 1000 заметок: запись (ревизия, чанкер §8.2, FTS,
/// задание outbox) — одна транзакция, после неё заметка уже находится поиском.
/// Запуск: `cargo test --release -p zam-store --test jobs -- --ignored --nocapture`.
#[tokio::test]
#[ignore]
async fn bench_write_until_fts_1000_notes() {
    let (db, k, _) = setup("bench-write").await;
    let para = "Сборка проекта идёт командой cargo build, тесты — cargo test. ".repeat(12);
    let mut lat = Vec::with_capacity(1000);
    for i in 0..1000 {
        let src = format!(
            "---\ntags: [bench]\n---\n# Заметка {i}\n\n{para}\n\n## Детали {i}\n\nметка{i} {para}\n"
        );
        let t = std::time::Instant::now();
        let id = add(&db, k, &format!("bench/{}/n{i}", i % 10), &src).await;
        let found = zam_store::search_fts(
            &db,
            zam_store::FtsQuery {
                text: format!("метка{i}"),
                space_ids: vec![k],
                path_prefix: None,
                top_k: 5,
            },
        )
        .await
        .unwrap();
        lat.push(t.elapsed());
        assert!(found.iter().any(|h| h.node_id == id));
    }
    lat.sort();
    let ms = |d: std::time::Duration| d.as_secs_f64() * 1000.0;
    println!(
        "запись→FTS, 1000 заметок: p50 {:.2} мс, p95 {:.2} мс, max {:.2} мс; заданий {}",
        ms(lat[500]),
        ms(lat[950]),
        ms(lat[999]),
        job_stats(&db).await.unwrap().queued
    );
}

/// R1 на 10 тысячах заметок M1: время фоновой перечанковки и задержка `get`.
/// Запуск: `cargo test --release -p zam-store --test jobs bench_r1 -- --ignored --nocapture`.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn bench_r1_rechunk_10000_notes() {
    const N: usize = 10_000;
    let dd = datadir("bench-r1");
    let user = Id::new();
    let path = dd.user_db(&user);
    let k = Id::new();
    {
        let db = Db::open(&path, &USER_MIGRATIONS[..3]).unwrap();
        db.write(move |tx| {
            let now = "2026-10-18T00:00:00.000000Z";
            tx.execute(
                "INSERT INTO spaces (id, kind, name, created_at) VALUES (?1, 'knowledge', 'Knowledge', ?2)",
                rusqlite::params![k, now],
            )?;
            let para = "Сборка проекта идёт командой cargo build, тесты — cargo test. ".repeat(8);
            for i in 0..N {
                let id = Id::new();
                let path = format!("d{}/n{i}", i % 50);
                let src = format!("# Заметка {i}\n\n{para}\n\n## Детали\n\n{para}\n\n## Ещё\n\n{para}\n");
                tx.execute(
                    "INSERT INTO nodes (id, space_id, kind, name, path, current_rev,
                                        index_state, created_at, updated_at)
                     VALUES (?1, ?2, 'note', ?3, ?4, 1, 'pending', ?5, ?5)",
                    rusqlite::params![id, k, format!("n{i}"), path, now],
                )?;
                tx.execute(
                    "INSERT INTO revisions (node_id, rev, content, content_hash, size,
                                            author_kind, ts)
                     VALUES (?1, 1, ?2, 'h', 1, 'agent', ?3)",
                    rusqlite::params![id, src, now],
                )?;
                tx.execute(
                    "INSERT INTO chunks (id, node_id, rev, level, ordinal, text,
                                         context_prefix, token_count, text_hash)
                     VALUES (?1, ?2, 1, 'section', 0, ?3, '', 3, 'x')",
                    rusqlite::params![id.to_string(), id, para],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    }
    let dbs = UserDbs::new(dd.users_dir());
    let t = std::time::Instant::now();
    let db = dbs.get(user).await.unwrap();
    let get_ms = t.elapsed().as_secs_f64() * 1000.0;
    // запись и чтение во время перечанковки не ждут её завершения
    let t2 = std::time::Instant::now();
    add(&db, k, "во-время-r1", NOTE).await;
    let write_ms = t2.elapsed().as_secs_f64() * 1000.0;
    dbs.wait_rechunk(user).await;
    let total = t.elapsed().as_secs_f64();
    let st = job_stats(&db).await.unwrap();
    println!(
        "R1, {N} заметок: get {get_ms:.1} мс (с миграцией), запись во время R1 {write_ms:.1} мс, перечанковка {total:.2} с, заданий {}",
        st.queued
    );
    assert_eq!(st.queued as usize, N + 1);
}

/// Возврат выдачи без неудачи (Qdrant недоступен): задание снова `queued`,
/// попытки и backoff не меняются; устаревшая выдача — `Stale`.
#[tokio::test]
async fn release_returns_job_without_attempt() {
    let (db, k, _) = setup("release").await;
    let n = add(&db, k, "r", NOTE).await;
    let now = Timestamp::now();
    let j = claim_jobs(&db, 8, now).await.unwrap().remove(0);
    assert_eq!(
        release_job(&db, j.id, j.claim_gen).await.unwrap(),
        JobOutcome::Requeued
    );
    assert_eq!(
        jobs_of(&db, n).await,
        [("upsert".into(), "queued".into(), 1)]
    );
    let again = claim_jobs(&db, 8, now).await.unwrap().remove(0);
    assert_eq!((again.id, again.attempts), (j.id, 0));
    assert_eq!(
        release_job(&db, j.id, j.claim_gen).await.unwrap(),
        JobOutcome::Stale
    );
    // правка во время обработки: release снимает dirty, следующая выдача берёт новую ревизию
    update_note(&db, n, 1, format!("{NOTE}\nещё\n"), None, attrib())
        .await
        .unwrap();
    release_job(&db, again.id, again.claim_gen).await.unwrap();
    let third = claim_jobs(&db, 8, now).await.unwrap().remove(0);
    assert_eq!(third.rev, 2);
    assert_eq!(
        complete_job(&db, third.id, third.claim_gen).await.unwrap(),
        JobOutcome::Done
    );
}

#[tokio::test]
async fn indexed_count_counts_live_indexed_notes() {
    let (db, k, _) = setup("indexed-count").await;
    assert_eq!(indexed_count(&db).await.unwrap(), 0);
    let a = add(&db, k, "a", NOTE).await;
    let b = add(&db, k, "b", NOTE).await;
    set_index_state(&db, a, 1, IndexState::Indexed)
        .await
        .unwrap();
    set_index_state(&db, b, 1, IndexState::Indexed)
        .await
        .unwrap();
    assert_eq!(indexed_count(&db).await.unwrap(), 2);
    delete_node(&db, b).await.unwrap();
    assert_eq!(indexed_count(&db).await.unwrap(), 1);
}
