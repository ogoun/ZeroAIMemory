//! Разбор идеи агентом (M6.5 §5, задача 2): транзакция [`review_idea`] — исходы `plan` (новая
//! задача или привязка к открытой), `return`, `decline`, `done`; условия (происхождение, статус
//! `new`, живая идея), пределы полей, проверка ссылок `basis`/`similar`, история разборов и
//! повтор по ключу.

use std::path::PathBuf;

use serde_json::{Value, json};
use zam_core::Id;
use zam_store::{
    AnswerBy, Attribution, AuthorKind, Channel, DataDir, Db, DecidedAs, DecidedVia, Entry,
    EntryKind, EntryPatch, EntryStatus, IdeaData, IdeaDecision, IdeaError, IdeaPriority,
    NewDecision, NewEntry, NewNote, NewReview, NodePath, REVIEW_MIN_CHARS_DEFAULT, REVIEW_REFS_MAX,
    REVIEWS_MAX, ReviewOutcome, ReviewTask, SpaceKind, StoreError, TaskData, create_entry,
    create_note, create_solution, decide_idea, delete_entry, entry_revisions, get_entry,
    list_spaces, planned_from_ideas, review_idea, review_queue, review_queue_head, update_entry,
};

// ---------- окружение ----------

fn datadir(name: &str) -> DataDir {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("ideas-review-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    DataDir::open(&dir).unwrap()
}

/// База пользователя: пространство решения и Knowledge.
async fn setup(name: &str) -> (Db, Id, Id) {
    let dd = datadir(name);
    let db = zam_store::UserDbs::new(dd.users_dir())
        .get(Id::new())
        .await
        .unwrap();
    let (s, _) = create_solution(&db, &format!("g-{name}"), "Решение")
        .await
        .unwrap();
    let kn = list_spaces(&db)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.kind == SpaceKind::Knowledge)
        .unwrap()
        .id;
    (db, s.id, kn)
}

/// Агент пользователя `user` в сессии `s-review`.
fn agent_of(user: Id) -> Attribution {
    Attribution {
        author_kind: AuthorKind::Agent,
        token_id: Some(Id::new()),
        session_id: Some("s-review".into()),
        machine: Some("box".into()),
        message: None,
        user_id: Some(user),
        area: None,
        solution: None,
    }
}

fn agent() -> Attribution {
    agent_of(Id::new())
}

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

fn entry(space: Id, kind: EntryKind, title: &str, body: &str, data: Value) -> NewEntry {
    NewEntry {
        refs: None,
        space_id: space,
        kind,
        title: title.into(),
        body: body.into(),
        data,
        area: Some("store".into()),
        tags: vec!["кэш".into()],
        status: None,
        certainty: None,
        idempotency_key: None,
        triggers: Vec::new(),
        links: Vec::new(),
    }
}

/// Новая идея с `data.origin` и `data.priority`.
async fn new_idea(db: &Db, space: Id, title: &str, origin: &str, priority: &str) -> Entry {
    let a = if origin == "agent" { agent() } else { human() };
    let data = json!({"origin": origin, "priority": priority});
    let e = entry(space, EntryKind::Idea, title, "Замысел идеи.", data);
    create_entry(db, e, &a).await.unwrap().0
}

/// Новая задача `open`.
async fn new_task(db: &Db, space: Id, title: &str) -> Entry {
    let e = entry(space, EntryKind::Task, title, "Шаги задачи.", json!({}));
    create_entry(db, e, &agent()).await.unwrap().0
}

async fn new_changelog(db: &Db, space: Id, title: &str) -> Entry {
    let e = entry(
        space,
        EntryKind::Changelog,
        title,
        "Сделано.",
        json!({"kind": "feat"}),
    );
    create_entry(db, e, &agent()).await.unwrap().0
}

async fn new_release(db: &Db, space: Id, version: &str) -> Entry {
    let e = entry(
        space,
        EntryKind::Release,
        version,
        "Выпуск.",
        json!({ "version": version }),
    );
    create_entry(db, e, &agent()).await.unwrap().0
}

/// Решение человека в WebUI (`decide`).
fn nd(decision: IdeaDecision) -> NewDecision {
    NewDecision {
        decision,
        horizon: None,
        reason: None,
        by: AnswerBy::Human,
        channel: Channel::Webui,
        task_space: None,
        request_solution: None,
        defer_days: 30,
        expected_origin: None,
    }
}

async fn new_note(db: &Db, space: Id, path: &str) -> Id {
    create_note(
        db,
        NewNote {
            space_id: space,
            path: NodePath::parse(path).unwrap(),
            content: "Заметка.".into(),
            l0: None,
            attrib: Attribution::new(AuthorKind::Human),
        },
    )
    .await
    .unwrap()
    .id
}

async fn set_task_status(db: &Db, task: Id, status: EntryStatus) {
    update_entry(
        db,
        task,
        None,
        EntryPatch {
            status: Some(status),
            reason: Some("по ходу работы".into()),
            ..EntryPatch::default()
        },
        &agent(),
    )
    .await
    .unwrap();
}

/// Правка тела идеи человеком (`returned` → `new`, M6.5 §6).
async fn human_edit(db: &Db, idea: Id, body: &str) -> Entry {
    update_entry(
        db,
        idea,
        None,
        EntryPatch {
            body: Some(body.into()),
            human_edit: true,
            ..EntryPatch::default()
        },
        &human(),
    )
    .await
    .unwrap()
}

/// Разбор для человека не короче предела по умолчанию: метка `tag` в начале.
fn summary(tag: &str) -> String {
    let s = format!(
        "Разбор {tag}. {}",
        "Идея просит кэш ответов /recall; в плане его нет, в ТЗ §9.4 есть только кэш выжимок. "
            .repeat(3)
    );
    assert!(s.chars().count() > REVIEW_MIN_CHARS_DEFAULT);
    s
}

/// Разбор с исходом `outcome` от пользователя `user`: только обязательные поля.
fn nr(outcome: ReviewOutcome, user: Id) -> NewReview {
    NewReview {
        outcome,
        summary: summary(outcome.as_str()),
        missing: None,
        basis: Vec::new(),
        similar: Vec::new(),
        task: ReviewTask::None,
        by_user: user,
        session: None,
        key: None,
        min_chars: REVIEW_MIN_CHARS_DEFAULT,
    }
}

fn data_of(e: &Entry) -> IdeaData {
    IdeaData::from_stored(&e.data)
}

/// Снимок записи для проверки «ничего не изменилось».
fn snap(e: &Entry) -> (i64, Option<EntryStatus>, Value) {
    (e.rev, e.status, e.data.clone())
}

/// Живые системные рёбра `derived_from` задача → идея.
async fn derived_edges(db: &Db, task: Id, idea: Id) -> i64 {
    db.read(move |c| {
        Ok(c.query_row(
            "SELECT count(*) FROM edges
             WHERE src_kind = 'entry' AND src_id = ?1 AND dst_kind = 'entry' AND dst_id = ?2
               AND type = 'derived_from' AND origin = 'system' AND deleted_at IS NULL",
            rusqlite::params![task, idea],
            |r| r.get(0),
        )?)
    })
    .await
    .unwrap()
}

async fn tasks_in(db: &Db, space: Id) -> i64 {
    db.read(move |c| {
        Ok(c.query_row(
            "SELECT count(*) FROM entries WHERE kind = 'task' AND space_id = ?1",
            [space],
            |r| r.get(0),
        )?)
    })
    .await
    .unwrap()
}

/// Системное ребро `derived_from` задача → идея прямым SQL (осталось от прежней задачи идеи).
async fn system_edge(db: &Db, space: Id, task: Id, idea: Id) {
    let now = zam_store::time::fmt_ts(jiff::Timestamp::now());
    db.write(move |tx| {
        tx.execute(
            "INSERT INTO edges (id, space_id, src_kind, src_id, dst_kind, dst_id, dst_space_id,
                                type, origin, created_at)
             VALUES (?1, ?2, 'entry', ?3, 'entry', ?4, ?2, 'derived_from', 'system', ?5)",
            rusqlite::params![Id::new(), space, task, idea, now],
        )?;
        Ok(())
    })
    .await
    .unwrap();
}

fn input_err(r: &Result<zam_store::Reviewed, IdeaError>, field: &str) -> bool {
    matches!(r, Err(IdeaError::ReviewInput(m)) if m.starts_with(&format!("{field}:")))
}

// ---------- исходы ----------

/// M6.5 §5 `plan`: новая задача `open` в пространстве идеи — заголовок идеи (или `task.title`),
/// тело — план шагов и строка «Идея: w:<id>», `data.priority` — приоритет идеи (или
/// `task.priority`), системное ребро `derived_from` задача → идея; идея `accepted` с `task_id`,
/// разбором (`review` и первым элементом `reviews`) и решением агента канала `review`.
#[tokio::test]
async fn review_plan_creates_open_task_with_priority() {
    let (db, sol, _) = setup("plan-new").await;
    let user = Id::new();
    let i = new_idea(&db, sol, "Кэш ответов", "human", "high").await;
    let mut r = nr(ReviewOutcome::Plan, user);
    r.task = ReviewTask::New {
        title: None,
        body: Some("1. Кэш в памяти.\n2. Сброс по записи.".into()),
        priority: None,
    };
    let out = review_idea(&db, i.id, r.clone(), &agent_of(user))
        .await
        .unwrap();
    assert!(!out.replay);
    let t = out.task.clone().unwrap();
    assert_eq!(
        (t.kind, t.status, t.title.as_str(), t.space_id),
        (EntryKind::Task, Some(EntryStatus::Open), "Кэш ответов", sol)
    );
    assert_eq!(
        TaskData::from_stored(&t.data).priority,
        Some(IdeaPriority::High)
    );
    assert!(t.body.starts_with("1. Кэш в памяти."), "{}", t.body);
    assert!(t.body.ends_with(&format!("Идея: w:{}", i.id)), "{}", t.body);
    assert_eq!(derived_edges(&db, t.id, i.id).await, 1);

    let e = get_entry(&db, i.id).await.unwrap();
    assert_eq!(e, out.idea);
    assert_eq!((e.status, e.rev), (Some(EntryStatus::Accepted), i.rev + 1));
    let d = data_of(&e);
    assert_eq!(d.task_id, Some(t.id));
    let rv = d.review.clone().unwrap();
    assert_eq!(rv.outcome, ReviewOutcome::Plan);
    assert_eq!(rv.task, Some(format!("k:{}", t.id)));
    assert_eq!(rv.summary, r.summary.trim());
    assert_eq!(rv.by_user, user);
    assert_eq!(rv.session.as_deref(), Some("s-review"));
    assert_eq!(d.reviews, vec![rv]);
    let dec = d.decided.unwrap();
    assert_eq!(
        (dec.decision, dec.by, dec.channel),
        (DecidedAs::Accept, AnswerBy::Agent, DecidedVia::Review)
    );
    assert_eq!(
        (
            &e.data["decided"]["decision"],
            &e.data["decided"]["by"],
            &e.data["decided"]["channel"]
        ),
        (&json!("accept"), &json!("agent"), &json!("review"))
    );
    assert_eq!(e.data["review"]["outcome"], "plan");
    // ревизия разбора — статус и `data` идеи
    let revs = entry_revisions(&db, i.id).await.unwrap();
    let last = revs.iter().max_by_key(|r| r.rev).unwrap();
    assert_eq!(
        (last.rev, last.status, &last.data),
        (e.rev, Some(EntryStatus::Accepted), &e.data)
    );

    // `task.title` и `task.priority` важнее заголовка и приоритета идеи
    let j = new_idea(&db, sol, "Вторая идея", "human", "high").await;
    let mut r = nr(ReviewOutcome::Plan, user);
    r.task = ReviewTask::New {
        title: Some("  Кэш ответов: этап 2  ".into()),
        body: None,
        priority: Some(IdeaPriority::Low),
    };
    let out = review_idea(&db, j.id, r, &agent_of(user)).await.unwrap();
    let t = out.task.unwrap();
    assert_eq!(t.title, "Кэш ответов: этап 2");
    assert_eq!(
        TaskData::from_stored(&t.data).priority,
        Some(IdeaPriority::Low)
    );
    // без `task.body` — тело идеи, как у «начать»
    assert!(t.body.starts_with("Замысел идеи."), "{}", t.body);
    assert!(t.body.ends_with(&format!("Идея: w:{}", j.id)));
}

/// M6.5 §5 `plan` с `task.id`: идею покрывает открытая задача плана — новой задачи нет, ребро
/// `derived_from` этой задачи на идею (у задачи может быть несколько идей; ребро, оставшееся от
/// прежней привязки, — не ошибка), идея `accepted` с `task_id` этой задачи. Задача не открытая —
/// [`IdeaError::ReviewTaskNotOpen`]; нет такой задачи, не задача или чужое пространство —
/// `ReviewInput` поля `task.id`; заголовок новой задачи длиннее 500 символов — `task.title`;
/// ничего не записано.
#[tokio::test]
async fn review_plan_links_existing_task() {
    let (db, sol, kn) = setup("plan-existing").await;
    let user = Id::new();
    let t = new_task(&db, sol, "Кэш ответов /recall").await;
    let before = tasks_in(&db, sol).await;
    let link = |id: Id| {
        let mut r = nr(ReviewOutcome::Plan, user);
        r.task = ReviewTask::Existing(id);
        r
    };
    let i = new_idea(&db, sol, "Кэш ответов", "human", "normal").await;
    let out = review_idea(&db, i.id, link(t.id), &agent_of(user))
        .await
        .unwrap();
    assert_eq!(out.task.as_ref().map(|x| x.id), Some(t.id));
    assert_eq!(tasks_in(&db, sol).await, before);
    assert_eq!(derived_edges(&db, t.id, i.id).await, 1);
    let d = data_of(&out.idea);
    assert_eq!(out.idea.status, Some(EntryStatus::Accepted));
    assert_eq!(d.task_id, Some(t.id));
    assert_eq!(d.review.unwrap().task, Some(format!("k:{}", t.id)));
    // вторая идея на ту же задачу, ребро уже есть (прежняя привязка) — без дубля
    let j = new_idea(&db, sol, "Кэш на 10 минут", "human", "normal").await;
    system_edge(&db, sol, t.id, j.id).await;
    let out = review_idea(&db, j.id, link(t.id), &agent_of(user))
        .await
        .unwrap();
    assert_eq!(data_of(&out.idea).task_id, Some(t.id));
    assert_eq!(derived_edges(&db, t.id, j.id).await, 1);
    assert_eq!(derived_edges(&db, t.id, i.id).await, 1);
    // задача `in_progress` и `blocked` — открытые
    for s in [EntryStatus::InProgress, EntryStatus::Blocked] {
        let u = new_task(&db, sol, s.as_str()).await;
        set_task_status(&db, u.id, s).await;
        let k = new_idea(&db, sol, s.as_str(), "human", "normal").await;
        let out = review_idea(&db, k.id, link(u.id), &agent_of(user))
            .await
            .unwrap();
        assert_eq!(out.idea.status, Some(EntryStatus::Accepted), "{s:?}");
    }

    let n = new_idea(&db, sol, "Отказы", "human", "normal").await;
    for s in [EntryStatus::Done, EntryStatus::Dropped] {
        let u = new_task(&db, sol, s.as_str()).await;
        set_task_status(&db, u.id, s).await;
        let r = review_idea(&db, n.id, link(u.id), &agent_of(user)).await;
        assert!(
            matches!(r, Err(IdeaError::ReviewTaskNotOpen)),
            "{s:?}: {r:?}"
        );
        assert_eq!(derived_edges(&db, u.id, n.id).await, 0);
    }
    let foreign = new_task(&db, kn, "Задача Knowledge").await;
    let gone = new_task(&db, sol, "Удалённая").await;
    delete_entry(&db, gone.id, None, &human()).await.unwrap();
    for bad in [foreign.id, gone.id, n.id, Id::new()] {
        let r = review_idea(&db, n.id, link(bad), &agent_of(user)).await;
        assert!(input_err(&r, "task.id"), "{bad}: {r:?}");
    }
    // заголовок новой задачи — как у записей, не длиннее 500 символов
    let mut long = nr(ReviewOutcome::Plan, user);
    long.task = ReviewTask::New {
        title: Some("я".repeat(501)),
        body: None,
        priority: None,
    };
    let r = review_idea(&db, n.id, long, &agent_of(user)).await;
    assert!(input_err(&r, "task.title"), "{r:?}");
    assert_eq!(snap(&get_entry(&db, n.id).await.unwrap()), snap(&n));
}

/// M6.5 §5 `return` — `returned` с `returned_at` и `missing` в разборе; без `missing` —
/// `ReviewInput("missing: …")`. `decline` — `declined`; `missing` у исхода не `return` и
/// `task` у исхода не `plan` — `ReviewInput`. Ошибка ничего не пишет.
#[tokio::test]
async fn review_return_and_decline() {
    let (db, sol, _) = setup("return-decline").await;
    let user = Id::new();
    let i = new_idea(&db, sol, "Кэш", "human", "normal").await;
    let r = review_idea(&db, i.id, nr(ReviewOutcome::Return, user), &agent_of(user)).await;
    assert!(input_err(&r, "missing"), "{r:?}");
    let mut blank = nr(ReviewOutcome::Return, user);
    blank.missing = Some("   ".into());
    let r = review_idea(&db, i.id, blank, &agent_of(user)).await;
    assert!(input_err(&r, "missing"), "{r:?}");
    let mut long = nr(ReviewOutcome::Return, user);
    long.missing = Some("я".repeat(2001));
    let r = review_idea(&db, i.id, long, &agent_of(user)).await;
    assert!(input_err(&r, "missing"), "{r:?}");
    assert_eq!(snap(&get_entry(&db, i.id).await.unwrap()), snap(&i));

    let mut ret = nr(ReviewOutcome::Return, user);
    ret.missing = Some(" Каких ответов и на какой срок? ".into());
    let out = review_idea(&db, i.id, ret, &agent_of(user)).await.unwrap();
    assert_eq!(out.idea.status, Some(EntryStatus::Returned));
    assert!(out.task.is_none());
    let d = data_of(&out.idea);
    assert!(d.returned_at.is_some());
    assert_eq!(d.task_id, None);
    let rv = d.review.unwrap();
    assert_eq!(
        (rv.outcome, rv.missing.as_deref(), rv.task),
        (
            ReviewOutcome::Return,
            Some("Каких ответов и на какой срок?"),
            None
        )
    );
    assert_eq!(d.decided.unwrap().decision, DecidedAs::Return);

    let j = new_idea(&db, sol, "Лишнее", "human", "normal").await;
    let mut m = nr(ReviewOutcome::Decline, user);
    m.missing = Some("что-то".into());
    let r = review_idea(&db, j.id, m, &agent_of(user)).await;
    assert!(input_err(&r, "missing"), "{r:?}");
    let mut t = nr(ReviewOutcome::Decline, user);
    t.task = ReviewTask::New {
        title: None,
        body: None,
        priority: None,
    };
    let r = review_idea(&db, j.id, t, &agent_of(user)).await;
    assert!(input_err(&r, "task"), "{r:?}");
    assert_eq!(snap(&get_entry(&db, j.id).await.unwrap()), snap(&j));
    let out = review_idea(&db, j.id, nr(ReviewOutcome::Decline, user), &agent_of(user))
        .await
        .unwrap();
    assert_eq!(out.idea.status, Some(EntryStatus::Declined));
    let d = data_of(&out.idea);
    assert_eq!(d.review.unwrap().outcome, ReviewOutcome::Decline);
    let dec = d.decided.unwrap();
    assert_eq!(
        (dec.decision, dec.channel),
        (DecidedAs::Decline, DecidedVia::Review)
    );
}

/// M6.5 §5 `done` — «уже сделано»: идея `done` с `done_at`; в `basis` нужна ссылка на сделанное
/// (`c:`, `commit:`, `k:` задачи `done`, `r:`), иначе `ReviewInput("basis: …сделанное…")`;
/// заметка и открытая задача сделанным не считаются.
#[tokio::test]
async fn review_done_requires_done_ref() {
    let (db, sol, _) = setup("done").await;
    let user = Id::new();
    let c = new_changelog(&db, sol, "Кэш ответов сделан").await;
    let note = new_note(&db, sol, "docs/cache.md").await;
    let open = new_task(&db, sol, "Ещё в работе").await;
    let finished = new_task(&db, sol, "Сделано").await;
    set_task_status(&db, finished.id, EntryStatus::Done).await;
    let done = |basis: Vec<String>| {
        let mut r = nr(ReviewOutcome::Done, user);
        r.basis = basis;
        r
    };

    let i = new_idea(&db, sol, "Кэш", "human", "normal").await;
    for basis in [
        vec![format!("n:{note}")],
        vec![format!("k:{}", open.id)],
        Vec::new(),
    ] {
        let r = review_idea(&db, i.id, done(basis.clone()), &agent_of(user)).await;
        assert!(
            matches!(&r, Err(IdeaError::ReviewInput(m))
                if m.starts_with("basis:") && m.contains("сделанное")),
            "{basis:?}: {r:?}"
        );
    }
    assert_eq!(snap(&get_entry(&db, i.id).await.unwrap()), snap(&i));
    let out = review_idea(
        &db,
        i.id,
        done(vec![format!("n:{note}"), format!("c:{}", c.id)]),
        &agent_of(user),
    )
    .await
    .unwrap();
    assert_eq!(out.idea.status, Some(EntryStatus::Done));
    let d = data_of(&out.idea);
    assert!(d.done_at.is_some());
    assert_eq!(
        d.review.unwrap().basis,
        vec![format!("n:{note}"), format!("c:{}", c.id)]
    );
    assert_eq!(d.decided.unwrap().decision, DecidedAs::Done);
    // `k:` задачи `done`, `r:` релиза и `commit:` — тоже сделанное
    let rel = new_release(&db, sol, "0.0.12-m6.5").await;
    for basis in [
        vec![format!("k:{}", finished.id)],
        vec![format!("r:{}", rel.id)],
        vec!["commit:0123abc".to_string()],
    ] {
        let j = new_idea(&db, sol, "Сделанное", "human", "normal").await;
        let out = review_idea(&db, j.id, done(basis.clone()), &agent_of(user))
            .await
            .unwrap();
        assert_eq!(out.idea.status, Some(EntryStatus::Done), "{basis:?}");
    }
}

/// У идеи `new` может остаться `task_id` снятой задачи (M5.7: `decline` из `accepted` его
/// оставляет, `reopen` из `declined` не переносит). Разбор переносит его в `prev_task_ids`:
/// переоткрытие той задачи идею, разобранную как `done`, не трогает (иначе хук статуса задачи
/// вернул бы её в `accepted`).
#[tokio::test]
async fn review_moves_stale_task_to_prev() {
    let (db, sol, _) = setup("stale-task").await;
    let user = Id::new();
    let i = new_idea(&db, sol, "Кэш", "human", "normal").await;
    let t = decide_idea(&db, i.id, nd(IdeaDecision::Accept), &human())
        .await
        .unwrap()
        .task
        .unwrap();
    set_task_status(&db, t.id, EntryStatus::Dropped).await;
    decide_idea(&db, i.id, nd(IdeaDecision::Decline), &human())
        .await
        .unwrap();
    let mut reopen = nd(IdeaDecision::Reopen);
    reopen.reason = Some("снова актуально".into());
    let back = decide_idea(&db, i.id, reopen, &human()).await.unwrap().idea;
    assert_eq!(back.status, Some(EntryStatus::New));
    assert_eq!(data_of(&back).task_id, Some(t.id), "предусловие M5.7");

    let c = new_changelog(&db, sol, "Кэш сделан").await;
    let mut r = nr(ReviewOutcome::Done, user);
    r.basis = vec![format!("c:{}", c.id)];
    let out = review_idea(&db, i.id, r, &agent_of(user)).await.unwrap();
    let d = data_of(&out.idea);
    assert_eq!(out.idea.status, Some(EntryStatus::Done));
    assert_eq!((d.task_id, d.prev_task_ids), (None, vec![t.id]));

    set_task_status(&db, t.id, EntryStatus::Open).await;
    let e = get_entry(&db, i.id).await.unwrap();
    assert_eq!(snap(&e), snap(&out.idea));
    assert_eq!(data_of(&e).prev_task_ids, vec![t.id]);
}

// ---------- условия и пределы ----------

/// M6.5 §5, условия: идея «от агента» — `AgentOrigin`; статус не `new` (в том числе второй
/// разбор той же идеи) — `Transition{to: "review"}`; удалённая — `IdeaDeleted`; не идея —
/// `NotIdea`; `summary` короче `min_chars` или длиннее 8000 символов — `ReviewInput("summary:
/// …")`. Ничего не записано.
#[tokio::test]
async fn review_preconditions() {
    let (db, sol, _) = setup("preconditions").await;
    let user = Id::new();
    let a = new_idea(&db, sol, "От агента", "agent", "normal").await;
    let r = review_idea(&db, a.id, nr(ReviewOutcome::Decline, user), &agent_of(user)).await;
    assert!(matches!(r, Err(IdeaError::AgentOrigin)), "{r:?}");
    assert_eq!(snap(&get_entry(&db, a.id).await.unwrap()), snap(&a));

    let h = new_idea(&db, sol, "Принята человеком", "human", "normal").await;
    let acc = decide_idea(&db, h.id, nd(IdeaDecision::Accept), &human())
        .await
        .unwrap();
    let r = review_idea(&db, h.id, nr(ReviewOutcome::Plan, user), &agent_of(user)).await;
    assert!(
        matches!(
            r,
            Err(IdeaError::Transition {
                from: EntryStatus::Accepted,
                to: "review"
            })
        ),
        "{r:?}"
    );
    assert_eq!(snap(&get_entry(&db, h.id).await.unwrap()), snap(&acc.idea));

    // второй разбор той же идеи (другая сессия) — переход не разрешён
    let s = new_idea(&db, sol, "Дважды", "human", "normal").await;
    review_idea(&db, s.id, nr(ReviewOutcome::Decline, user), &agent_of(user))
        .await
        .unwrap();
    let r = review_idea(&db, s.id, nr(ReviewOutcome::Decline, user), &agent()).await;
    assert!(
        matches!(
            r,
            Err(IdeaError::Transition {
                from: EntryStatus::Declined,
                to: "review"
            })
        ),
        "{r:?}"
    );

    let g = new_idea(&db, sol, "Удалена", "human", "normal").await;
    delete_entry(&db, g.id, None, &human()).await.unwrap();
    let r = review_idea(&db, g.id, nr(ReviewOutcome::Decline, user), &agent_of(user)).await;
    assert!(matches!(r, Err(IdeaError::IdeaDeleted)), "{r:?}");

    let t = new_task(&db, sol, "Не идея").await;
    let r = review_idea(&db, t.id, nr(ReviewOutcome::Decline, user), &agent_of(user)).await;
    assert!(matches!(r, Err(IdeaError::NotIdea)), "{r:?}");

    let i = new_idea(&db, sol, "Короткий разбор", "human", "normal").await;
    for (text, n) in [
        ("я".repeat(199), "200"),
        ("  ".to_string(), "200"),
        ("я".repeat(8001), "8000"),
    ] {
        let mut r = nr(ReviewOutcome::Decline, user);
        r.summary = text;
        let r = review_idea(&db, i.id, r, &agent_of(user)).await;
        assert!(
            matches!(&r, Err(IdeaError::ReviewInput(m)) if m.starts_with("summary:") && m.contains(n)),
            "{r:?}"
        );
    }
    // разбор обязателен и при нулевом пределе вызывающего
    let mut r = nr(ReviewOutcome::Decline, user);
    r.summary = " ".into();
    r.min_chars = 0;
    let r = review_idea(&db, i.id, r, &agent_of(user)).await;
    assert!(input_err(&r, "summary"), "{r:?}");
    assert_eq!(snap(&get_entry(&db, i.id).await.unwrap()), snap(&i));
    // предел — в символах, не в байтах; `min_chars` задаёт вызывающий
    let mut r = nr(ReviewOutcome::Decline, user);
    r.summary = "я".repeat(50);
    r.min_chars = 50;
    let out = review_idea(&db, i.id, r, &agent_of(user)).await.unwrap();
    assert_eq!(out.idea.status, Some(EntryStatus::Declined));
}

/// M6.5 §5, история: разборы — в `data.reviews`, новые в начале, не больше [`REVIEWS_MAX`] (10);
/// `data.review` — последний (`reviews[0]`). Цикл: `return` → правка человеком → `return`.
#[tokio::test]
async fn review_history_keeps_ten() {
    assert_eq!(REVIEWS_MAX, 10);
    let (db, sol, _) = setup("history").await;
    let user = Id::new();
    let i = new_idea(&db, sol, "Долгая идея", "human", "normal").await;
    for n in 1..=12 {
        if n > 1 {
            let e = human_edit(&db, i.id, &format!("Тело, правка {n}.")).await;
            assert_eq!(e.status, Some(EntryStatus::New), "{n}");
        }
        let mut r = nr(ReviewOutcome::Return, user);
        r.summary = summary(&format!("№{n}"));
        r.missing = Some(format!("Вопрос {n}"));
        let out = review_idea(&db, i.id, r, &agent_of(user)).await.unwrap();
        assert_eq!(out.idea.status, Some(EntryStatus::Returned), "{n}");
    }
    let d = data_of(&get_entry(&db, i.id).await.unwrap());
    assert_eq!(d.reviews.len(), 10);
    assert!(d.reviews[0].summary.starts_with("Разбор №12."));
    assert!(d.reviews[9].summary.starts_with("Разбор №3."));
    assert_eq!(d.review.as_ref(), d.reviews.first());
}

/// M6.5 §5, Review Focus 1: повтор того же разбора с тем же ключом после обрыва связи — прежний
/// итог (`replay: true`) с той же задачей, без новой ревизии и второй задачи. Тот же ключ другого
/// пользователя — не повтор (ключи у каждого свои; обычные условия: идея уже не `new` — 409); тот
/// же ключ того же пользователя с другим исходом — другой запрос с тем же ключом:
/// `IdempotencyMismatch` (финальное ревью ветки, M1).
#[tokio::test]
async fn review_retry_same_key_returns_same_task() {
    let (db, sol, _) = setup("replay").await;
    let user = Id::new();
    let i = new_idea(&db, sol, "Кэш", "human", "normal").await;
    let mut r = nr(ReviewOutcome::Plan, user);
    r.key = Some("rk-1".into());
    r.task = ReviewTask::New {
        title: None,
        body: Some("1. Шаг.".into()),
        priority: None,
    };
    let first = review_idea(&db, i.id, r.clone(), &agent_of(user))
        .await
        .unwrap();
    assert!(!first.replay);
    assert_eq!(
        data_of(&first.idea).review.unwrap().key.as_deref(),
        Some("rk-1")
    );
    let revs = entry_revisions(&db, i.id).await.unwrap().len();
    let again = review_idea(&db, i.id, r.clone(), &agent_of(user))
        .await
        .unwrap();
    assert!(again.replay);
    assert_eq!(again.idea, first.idea);
    assert_eq!(again.task.map(|t| t.id), first.task.as_ref().map(|t| t.id));
    assert_eq!(tasks_in(&db, sol).await, 1);
    assert_eq!(entry_revisions(&db, i.id).await.unwrap().len(), revs);
    // чужой пользователь с тем же ключом — не повтор: обычные условия, идея уже не `new` —
    // переход не разрешён; тот же ключ с другим исходом — расхождение ключа; ничего не записано
    let mut other_user = r.clone();
    other_user.by_user = Id::new();
    let res = review_idea(&db, i.id, other_user, &agent()).await;
    assert!(
        matches!(
            res,
            Err(IdeaError::Transition {
                from: EntryStatus::Accepted,
                to: "review"
            })
        ),
        "{res:?}"
    );
    let mut other_outcome = nr(ReviewOutcome::Decline, user);
    other_outcome.key = r.key.clone();
    let res = review_idea(&db, i.id, other_outcome, &agent_of(user)).await;
    assert!(
        matches!(
            res,
            Err(IdeaError::Store(StoreError::IdempotencyMismatch(_)))
        ),
        "{res:?}"
    );
    assert_eq!(tasks_in(&db, sol).await, 1);
    assert_eq!(entry_revisions(&db, i.id).await.unwrap().len(), revs);
    assert_eq!(get_entry(&db, i.id).await.unwrap(), first.idea);
}

/// Финальное ревью ветки, M1: повтор по ключу — только пока идея не менялась после разбора с
/// этим ключом. Разбор K (`return`) → человек дописал (`new`) → тот же разбор с K —
/// `IdempotencyMismatch` («ключ уже использован прежним разбором»), ничего не записано, идея
/// остаётся в очереди; новый ключ — обычный разбор. Ключ прежнего разбора из истории
/// (`data.reviews`, не последний) — тоже расхождение.
#[tokio::test]
async fn review_key_after_change_is_mismatch() {
    let (db, sol, _) = setup("replay-changed").await;
    let user = Id::new();
    let i = new_idea(&db, sol, "Кэш", "human", "normal").await;
    let ret = |key: &str| {
        let mut r = nr(ReviewOutcome::Return, user);
        r.missing = Some("Какой срок жизни кэша?".into());
        r.key = Some(key.into());
        r
    };
    let first = review_idea(&db, i.id, ret("rk-a"), &agent_of(user))
        .await
        .unwrap();
    assert!(!first.replay);
    assert_eq!(first.idea.status, Some(EntryStatus::Returned));
    // без правки — повтор
    let again = review_idea(&db, i.id, ret("rk-a"), &agent_of(user))
        .await
        .unwrap();
    assert!(again.replay);
    let edited = human_edit(&db, i.id, "Кэш на час.").await;
    assert_eq!(edited.status, Some(EntryStatus::New));
    let mismatch = |res: &Result<zam_store::Reviewed, IdeaError>| match res {
        Err(IdeaError::Store(StoreError::IdempotencyMismatch(m))) => {
            assert!(m.contains("ключ уже использован прежним разбором"), "{m}");
            assert!(m.contains("новый разбор — с новым ключом"), "{m}");
        }
        other => panic!("ожидалось IdempotencyMismatch: {other:?}"),
    };
    mismatch(&review_idea(&db, i.id, ret("rk-a"), &agent_of(user)).await);
    let cur = get_entry(&db, i.id).await.unwrap();
    assert_eq!(snap(&cur), snap(&edited));
    assert_eq!(review_queue(&db, sol, 10).await.unwrap().0, 1);
    // новый ключ — обычный разбор
    let second = review_idea(&db, i.id, ret("rk-b"), &agent_of(user))
        .await
        .unwrap();
    assert!(!second.replay);
    assert_eq!(data_of(&second.idea).reviews.len(), 2);
    // ключ прежнего разбора (уже не последнего) после новой правки — расхождение
    human_edit(&db, i.id, "Кэш на два часа.").await;
    mismatch(&review_idea(&db, i.id, ret("rk-a"), &agent_of(user)).await);
    // другой пользователь со своим ключом «rk-a» — обычный разбор (ключи у каждого свои)
    let mut other = ret("rk-a");
    other.by_user = Id::new();
    let r = review_idea(&db, i.id, other, &agent()).await.unwrap();
    assert!(!r.replay);
}

/// M6.5 §5, Review Focus 4: мусор в ссылках — `ReviewInput` с названием поля, ничего не записано:
/// неизвестный id, чужое пространство, буква не того вида, без префикса, `file:` с `..` или
/// абсолютный, `commit:` не hex 7–64, больше 50 ссылок в поле; в `similar` — только идеи `w:`
/// (отклонённая — можно). Принятые ссылки хранятся нормализованными (канонический id с префиксом,
/// `file:` с `/`, `commit:` в нижнем регистре), повторы — один раз.
#[tokio::test]
async fn review_rejects_bad_refs() {
    let (db, sol, kn) = setup("bad-refs").await;
    let user = Id::new();
    let i = new_idea(&db, sol, "Ссылки", "human", "normal").await;
    let other = new_idea(&db, sol, "Похожая", "human", "normal").await;
    let task = new_task(&db, sol, "Задача").await;
    let foreign = new_task(&db, kn, "Задача Knowledge").await;
    let with = |basis: Vec<String>, similar: Vec<String>| {
        let mut r = nr(ReviewOutcome::Decline, user);
        r.basis = basis;
        r.similar = similar;
        r
    };
    for bad in [
        format!("k:{}", Id::new()),
        "зачем".to_string(),
        "file:../x".to_string(),
        "file:docs/../../x".to_string(),
        "file:/etc/passwd".to_string(),
        "file:C:/x".to_string(),
        "commit:zz".to_string(),
        "commit:abc12".to_string(),
        format!("commit:{}", "a".repeat(65)),
        format!("k:{}", foreign.id),
        format!("w:{}", task.id),
        "k:не-id".to_string(),
        format!("p:{}", task.id),
        "x:1".to_string(),
    ] {
        let r = review_idea(
            &db,
            i.id,
            with(vec![bad.clone()], Vec::new()),
            &agent_of(user),
        )
        .await;
        assert!(input_err(&r, "basis"), "{bad}: {r:?}");
    }
    for bad in [
        format!("k:{}", task.id),
        format!("w:{}", Id::new()),
        format!("w:{}", i.id),
    ] {
        let r = review_idea(
            &db,
            i.id,
            with(Vec::new(), vec![bad.clone()]),
            &agent_of(user),
        )
        .await;
        assert!(input_err(&r, "similar"), "{bad}: {r:?}");
    }
    // больше [`REVIEW_REFS_MAX`] ссылок в поле — ошибка поля; ровно столько — можно
    let files =
        |n: usize| -> Vec<String> { (0..n).map(|k| format!("file:docs/f{k}.md")).collect() };
    assert_eq!(REVIEW_REFS_MAX, 50);
    let r = review_idea(&db, i.id, with(files(51), Vec::new()), &agent_of(user)).await;
    assert!(
        matches!(&r, Err(IdeaError::ReviewInput(m)) if m.starts_with("basis:") && m.contains("50")),
        "{r:?}"
    );
    let r = review_idea(&db, i.id, with(Vec::new(), files(51)), &agent_of(user)).await;
    assert!(input_err(&r, "similar"), "{r:?}");
    assert_eq!(snap(&get_entry(&db, i.id).await.unwrap()), snap(&i));
    let many = new_idea(&db, sol, "Много ссылок", "human", "normal").await;
    let out = review_idea(&db, many.id, with(files(50), Vec::new()), &agent_of(user))
        .await
        .unwrap();
    assert_eq!(data_of(&out.idea).review.unwrap().basis.len(), 50);

    // похожая идея — в том числе отклонённая (образец «уже обсуждали», §6)
    decide_idea(&db, other.id, nd(IdeaDecision::Decline), &human())
        .await
        .unwrap();
    let out = review_idea(
        &db,
        i.id,
        with(
            vec![
                format!(" k:{} ", task.id.as_uuid()),
                format!("k:{}", task.id),
                "file:docs\\tz.md".into(),
                "commit:ABCDEF1".into(),
                format!("w:{}", other.id),
            ],
            vec![format!("w:{}", other.id)],
        ),
        &agent_of(user),
    )
    .await
    .unwrap();
    let rv = data_of(&out.idea).review.unwrap();
    assert_eq!(
        rv.basis,
        vec![
            format!("k:{}", task.id),
            "file:docs/tz.md".to_string(),
            "commit:abcdef1".to_string(),
            format!("w:{}", other.id),
        ]
    );
    assert_eq!(rv.similar, vec![format!("w:{}", other.id)]);
}

// ---------- очередь разбора и строка плана (M6.5 §7, задача 4) ----------

/// Очередь разбора: живые идеи человека `new` пространства — `high` первыми, затем самые старые;
/// без идей агента, разобранных, удалённых и чужого пространства; предел `limit`, длина очереди
/// — без предела (тем же чтением).
#[tokio::test]
async fn review_queue_order_and_filters() {
    let (db, sol, kn) = setup("queue").await;
    let old = new_idea(&db, sol, "Старая", "human", "normal").await;
    let low = new_idea(&db, sol, "Низкая", "human", "low").await;
    let high = new_idea(&db, sol, "Срочная", "human", "high").await;
    let young = new_idea(&db, sol, "Новая", "human", "normal").await;
    new_idea(&db, sol, "Идея агента", "agent", "high").await;
    new_idea(&db, kn, "Идея Knowledge", "human", "high").await;
    let gone = new_idea(&db, sol, "Удалённая", "human", "high").await;
    delete_entry(&db, gone.id, None, &human()).await.unwrap();
    let done = new_idea(&db, sol, "Разобранная", "human", "high").await;
    review_idea(
        &db,
        done.id,
        nr(ReviewOutcome::Decline, Id::new()),
        &agent(),
    )
    .await
    .unwrap();
    let ids = |(n, v): (usize, Vec<Entry>)| (n, v.into_iter().map(|e| e.id).collect::<Vec<_>>());
    assert_eq!(
        ids(review_queue(&db, sol, 10).await.unwrap()),
        (4, vec![high.id, old.id, young.id, low.id])
    );
    assert_eq!(
        ids(review_queue(&db, sol, 2).await.unwrap()),
        (4, vec![high.id, old.id])
    );
    assert_eq!(ids(review_queue(&db, sol, 0).await.unwrap()), (4, vec![]));
    // очередь — одного пространства: идея Knowledge в очередь решения не попадает
    assert_eq!(review_queue(&db, kn, 10).await.unwrap().0, 1);
}

/// Голова очереди к строке очереди разбора (M7.1 §4.5 п. 1): длина очереди, первая идея по
/// порядку очереди (`high` первыми, затем самые старые) и время последней ревизии идей очереди;
/// идеи вне очереди (агента, Knowledge, разобранные) ни в длину, ни во время не идут; новая
/// ревизия идеи очереди время двигает; пустая очередь — 0 без первой и без времени.
#[tokio::test]
async fn review_queue_head_first_and_changed_at() {
    let (db, sol, kn) = setup("queue-head").await;
    let h = review_queue_head(&db, sol).await.unwrap();
    assert_eq!((h.total, h.first.is_none(), h.changed_at), (0, true, None));
    let stored = |e: &Entry| {
        let (db, id) = (db.clone(), e.id);
        async move { get_entry(&db, id).await.unwrap().updated_at }
    };
    let old = new_idea(&db, sol, "Старая", "human", "normal").await;
    let young = new_idea(&db, sol, "Новая", "human", "normal").await;
    let h = review_queue_head(&db, sol).await.unwrap();
    assert_eq!((h.total, h.first.map(|e| e.id)), (2, Some(old.id)));
    assert_eq!(h.changed_at, Some(stored(&young).await));
    let high = new_idea(&db, sol, "Срочная", "human", "high").await;
    new_idea(&db, sol, "Идея агента", "agent", "high").await;
    new_idea(&db, kn, "Идея Knowledge", "human", "high").await;
    let h = review_queue_head(&db, sol).await.unwrap();
    assert_eq!(h.total, 3);
    assert_eq!(h.first.map(|e| e.title), Some("Срочная".to_string()));
    assert_eq!(h.changed_at, Some(stored(&high).await));
    // новая ревизия идеи очереди (правка человеком) — время её ревизии
    let edited = human_edit(&db, old.id, "Замысел идеи с подробностями.").await;
    let h = review_queue_head(&db, sol).await.unwrap();
    assert_eq!(h.changed_at, Some(stored(&edited).await));
    // разобранная — вне очереди
    review_idea(
        &db,
        high.id,
        nr(ReviewOutcome::Decline, Id::new()),
        &agent(),
    )
    .await
    .unwrap();
    let h = review_queue_head(&db, sol).await.unwrap();
    assert_eq!((h.total, h.first.map(|e| e.id)), (2, Some(old.id)));
    assert_eq!(review_queue_head(&db, kn).await.unwrap().total, 1);
}

/// Строка плана: живые задачи `open | in_progress | blocked` с системным ребром `derived_from`
/// на живую идею; следующая — только среди `open`, по `data.priority` (нет — `normal`), затем
/// самая старая (ревью задачи 4, Important 2: взятая в работу «следующей» не бывает); открытых
/// нет — `(N, None)`. Задача без ребра, снятая, сделанная и с ребром только на удалённую идею не
/// считаются.
#[tokio::test]
async fn planned_from_ideas_next_by_priority() {
    let (db, sol, _) = setup("planned").await;
    assert_eq!(planned_from_ideas(&db, sol).await.unwrap().0, 0);
    let plan = |priority: Option<IdeaPriority>| {
        let mut r = nr(ReviewOutcome::Plan, Id::new());
        r.task = ReviewTask::New {
            title: None,
            body: Some("1. Шаг.".into()),
            priority,
        };
        r
    };
    let planned = |title: &'static str, priority: Option<IdeaPriority>| {
        let db = db.clone();
        async move {
            let i = new_idea(&db, sol, title, "human", "normal").await;
            review_idea(&db, i.id, plan(priority), &agent())
                .await
                .unwrap()
                .task
                .unwrap()
        }
    };
    // задача «начать» M5.7 без `data.priority` — как `normal`
    let started = new_idea(&db, sol, "Начатая", "human", "low").await;
    let first = decide_idea(&db, started.id, nd(IdeaDecision::Accept), &human())
        .await
        .unwrap()
        .task
        .unwrap();
    assert_eq!(TaskData::from_stored(&first.data).priority, None);
    planned("Низкая", Some(IdeaPriority::Low)).await;
    planned("Обычная", Some(IdeaPriority::Normal)).await;
    let (n, next) = planned_from_ideas(&db, sol).await.unwrap();
    assert_eq!(
        (n, next.unwrap().id),
        (3, first.id),
        "без приоритета — normal, старше"
    );
    let high = planned("Срочная", Some(IdeaPriority::High)).await;
    let (n, next) = planned_from_ideas(&db, sol).await.unwrap();
    assert_eq!((n, next.unwrap().id), (4, high.id), "high — первой");
    // взятая в работу — в счёте, но не «следующая»
    set_task_status(&db, high.id, EntryStatus::InProgress).await;
    let (n, next) = planned_from_ideas(&db, sol).await.unwrap();
    assert_eq!((n, next.unwrap().id), (4, first.id));
    // не считаются: без ребра, снятая, сделанная, с ребром только на удалённую идею
    new_task(&db, sol, "Ручная").await;
    let dropped = planned("Снятая", Some(IdeaPriority::High)).await;
    set_task_status(&db, dropped.id, EntryStatus::Dropped).await;
    let done = planned("Сделанная", Some(IdeaPriority::High)).await;
    set_task_status(&db, done.id, EntryStatus::Done).await;
    let orphan = new_task(&db, sol, "Сирота").await;
    let gone = new_idea(&db, sol, "Удалённая", "human", "high").await;
    system_edge(&db, sol, orphan.id, gone.id).await;
    delete_entry(&db, gone.id, None, &human()).await.unwrap();
    let (n, next) = planned_from_ideas(&db, sol).await.unwrap();
    assert_eq!((n, next.unwrap().id), (4, first.id));
    // открытых не осталось (в работе и заблокированы) — пункты есть, «следующей» нет
    let open: Vec<Id> = db
        .read(move |c| {
            let mut st = c.prepare(
                "SELECT id FROM entries WHERE space_id = ?1 AND kind = 'task' AND status = 'open'",
            )?;
            let ids = st.query_map([sol], |r| r.get::<_, Id>(0))?;
            Ok(ids.collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .unwrap();
    for (k, t) in open.iter().enumerate() {
        let to = if k % 2 == 0 {
            EntryStatus::InProgress
        } else {
            EntryStatus::Blocked
        };
        set_task_status(&db, *t, to).await;
    }
    let (n, next) = planned_from_ideas(&db, sol).await.unwrap();
    assert_eq!((n, next.map(|e| e.id)), (4, None));
}
