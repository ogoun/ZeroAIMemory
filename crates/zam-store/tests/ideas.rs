//! Журнал «Идеи» (M5.7, задача 1): вид записи `idea`, данные, переходы §5 спецификации,
//! предложение (`propose`), решение (`decide`), ответ на вопрос-предложение, «принять как свою»,
//! связь с задачей, корзина, отбор кандидатов и лента. Наступивший `horizon` и «старые» моменты
//! правки задаются прямым SQL — реального ожидания нет.

use std::collections::HashSet;
use std::path::PathBuf;

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use proptest::prelude::*;
use serde_json::{Value, json};
use zam_core::{Id, TargetKind, TargetRef};
use zam_store::{
    AnswerBy, Answered, Attribution, AuthorKind, Channel, DataDir, Db, EdgeType, Entry,
    EntryFilter, EntryKind, EntryPatch, EntryStatus, GraphStore, IDEA_LINKS_MAX, IDEA_OPTIONS,
    IdeaData, IdeaDecision, IdeaError, IdeaOrigin, IdeaPriority, NewAnswer, NewDecision, NewEdge,
    NewEntry, QuestionData, QuestionError, SpaceKind, StoreError, TaskData, TaskState, UserDbs,
    adopt_idea, answer_question, check_idea, check_idea_transition, create_entry, create_solution,
    decide_idea, delete_entry, get_entry, idea_candidate, list_entries, list_spaces,
    proactive_ideas, propose_idea, restore_entry, update_entry,
};

// ---------- окружение ----------

fn datadir(name: &str) -> DataDir {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("ideas-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    DataDir::open(&dir).unwrap()
}

/// База пользователя: пространство решения и Knowledge.
async fn setup(name: &str) -> (Db, Id, Id) {
    let dd = datadir(name);
    let db = UserDbs::new(dd.users_dir()).get(Id::new()).await.unwrap();
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

fn agent() -> Attribution {
    Attribution {
        author_kind: AuthorKind::Agent,
        token_id: Some(Id::new()),
        session_id: Some("s-idea".into()),
        machine: Some("box".into()),
        message: None,
        user_id: Some(Id::new()),
        area: None,
        solution: None,
    }
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

fn entry_ref(id: Id) -> TargetRef {
    TargetRef {
        kind: TargetKind::Entry,
        id,
    }
}

fn idea_entry(space: Id, title: &str, data: Value) -> NewEntry {
    NewEntry {
        refs: None,
        space_id: space,
        kind: EntryKind::Idea,
        title: title.into(),
        body: "Замысел идеи.".into(),
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
    create_entry(db, idea_entry(space, title, data), &a)
        .await
        .unwrap()
        .0
}

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

fn answer(option: Option<&str>, text: Option<&str>, by: AnswerBy) -> NewAnswer {
    NewAnswer {
        text: text.map(str::to_string),
        option: option.map(str::to_string),
        by,
        channel: Channel::Chat,
        horizon: None,
    }
}

fn today() -> Date {
    Timestamp::now().to_zoned(TimeZone::UTC).date()
}

fn days(n: i64) -> jiff::Span {
    jiff::Span::new().days(n)
}

async fn count(db: &Db, sql: &'static str) -> i64 {
    db.read(move |c| Ok(c.query_row(sql, [], |r| r.get(0))?))
        .await
        .unwrap()
}

async fn count_id(db: &Db, sql: &'static str, id: Id) -> i64 {
    db.read(move |c| Ok(c.query_row(sql, [id], |r| r.get(0))?))
        .await
        .unwrap()
}

async fn exec(db: &Db, sql: String) -> Result<usize, StoreError> {
    db.write(move |tx| Ok(tx.execute(&sql, [])?)).await
}

const DECISIONS: &str = "SELECT count(*) FROM entries
    WHERE kind = 'insight' AND json_extract(data, '$.kind') = 'decision'";

fn data_of(e: &Entry) -> IdeaData {
    IdeaData::from_stored(&e.data)
}

/// Снимок записи для проверки «ничего не изменилось».
fn snap(e: &Entry) -> (i64, Option<EntryStatus>, Value) {
    (e.rev, e.status, e.data.clone())
}

// ---------- данные ----------

#[test]
fn idea_data_rules() {
    use IdeaOrigin::*;
    let d = check_idea(&json!({}), Human).unwrap();
    assert_eq!(d.priority, IdeaPriority::Normal);
    assert_eq!(d.origin, Human);
    assert_eq!(d.to_value()["priority"], "normal");
    assert_eq!(d.to_value()["origin"], "human");
    // origin в data совпадает с решённым сервером — допустимо, расходится — ошибка
    assert!(check_idea(&json!({"origin": "human"}), Human).is_ok());
    assert!(check_idea(&json!({"origin": "agent"}), Human).is_err());
    for bad in ["завтра", "2026-13-01", "2026-11-04T00:00:00Z"] {
        let e = check_idea(&json!({ "horizon": bad }), Human).unwrap_err();
        assert!(e.contains("data.horizon"), "{bad}: {e}");
    }
    let ok = check_idea(
        &json!({"horizon": "2099-01-02", "links": ["a", " a ", ""], "priority": "high"}),
        Human,
    )
    .unwrap();
    assert_eq!(ok.horizon, Some(jiff::civil::date(2099, 1, 2)));
    assert_eq!(ok.links, vec!["a"]);
    assert_eq!(ok.priority, IdeaPriority::High);
    let many: Vec<String> = (0..=IDEA_LINKS_MAX).map(|i| format!("l{i}")).collect();
    assert_eq!(many.len(), 51);
    let e = check_idea(&json!({ "links": many }), Human).unwrap_err();
    assert!(e.contains("data.links"), "{e}");
    assert!(check_idea(&json!({"links": ["x".repeat(1001)]}), Human).is_err());
    assert!(check_idea(&json!({"priority": "urgent"}), Human).is_err());
    assert!(check_idea(&json!({"foo": 1}), Human).is_err());
    for k in [
        "task_id",
        "prev_task_ids",
        "question_id",
        "proposed_at",
        "decided",
        "adopted_at",
        "adopted_by",
        "done_at",
    ] {
        let e = check_idea(&json!({ k: null }), Human).unwrap_err();
        assert!(e.contains("поле ведёт сервер"), "{k}: {e}");
    }
    // патч: origin и поля сервера — нет, null снимает поле
    let mut d = ok.clone();
    let e = d.apply_patch(&json!({"origin": "human"})).unwrap_err();
    assert!(e.contains("data.origin"), "{e}");
    let e = d
        .apply_patch(&json!({"task_id": Id::new().to_string()}))
        .unwrap_err();
    assert!(e.contains("поле ведёт сервер"), "{e}");
    assert_eq!(d, ok, "ошибка патча не оставляет частичного результата");
    d.apply_patch(&json!({"horizon": null, "priority": "low"}))
        .unwrap();
    assert_eq!((d.horizon, d.priority), (None, IdeaPriority::Low));
    d.apply_patch(&json!({"links": null, "priority": null}))
        .unwrap();
    assert!(d.links.is_empty());
    assert_eq!(d.priority, IdeaPriority::Normal);
    assert_eq!(IdeaData::from_stored(&ok.to_value()), ok);
    // повреждённые поля — умолчания; происхождение по умолчанию — agent (не предлагается)
    let broken = IdeaData::from_stored(&json!({"priority": 5, "origin": "robot", "links": 1}));
    assert_eq!(broken.priority, IdeaPriority::Normal);
    assert_eq!(broken.origin, Agent);
    assert!(broken.links.is_empty());
    assert!(
        IdeaPriority::High.rank() > IdeaPriority::Normal.rank()
            && IdeaPriority::Normal.rank() > IdeaPriority::Low.rank()
    );
    assert_eq!(
        IDEA_OPTIONS,
        [
            ("accept", "начать"),
            ("defer", "отложить"),
            ("decline", "отказаться")
        ]
    );
    for d in [
        IdeaDecision::Accept,
        IdeaDecision::Defer,
        IdeaDecision::Decline,
        IdeaDecision::Reopen,
    ] {
        assert_eq!(IdeaDecision::parse(d.as_str()), Some(d));
    }
    assert_eq!(IdeaDecision::Reopen.option_key(), None);
    assert_eq!(IdeaDecision::Defer.option_key(), Some("defer"));
}

#[tokio::test]
async fn idea_status_check_in_db() {
    let (db, sol, _) = setup("check").await;
    let i = new_idea(&db, sol, "Идея", "human", "normal").await;
    assert_eq!(i.status, Some(EntryStatus::New));
    assert_eq!(i.kind.ref_letter(), "w");
    let r = exec(
        &db,
        format!("UPDATE entries SET status = 'open' WHERE id = '{}'", i.id),
    )
    .await;
    assert!(
        matches!(&r, Err(StoreError::Sqlite(e)) if e.to_string().contains("CHECK")),
        "{r:?}"
    );
    exec(
        &db,
        format!(
            "UPDATE entries SET status = 'deferred' WHERE id = '{}'",
            i.id
        ),
    )
    .await
    .unwrap();
    // токену без data.origin — ошибка с подсказкой
    let r = create_entry(&db, idea_entry(sol, "Без origin", json!({})), &agent()).await;
    assert!(
        matches!(&r, Err(StoreError::BadInput(m)) if m.contains("data.origin")),
        "{r:?}"
    );
    // правка статуса идеи — только операциями
    let r = update_entry(
        &db,
        i.id,
        None,
        EntryPatch {
            status: Some(EntryStatus::Accepted),
            ..EntryPatch::default()
        },
        &human(),
    )
    .await;
    assert!(
        matches!(&r, Err(StoreError::BadInput(m)) if m.contains("propose, decide, adopt")),
        "{r:?}"
    );
}

/// Повтор создания с тем же `Idempotency-Key` — та же идея: `data` хранится нормализованной,
/// а запрос сравнивается в том же виде.
#[tokio::test]
async fn idea_idempotency_key_repeat() {
    let (db, sol, _) = setup("idem").await;
    let a = agent();
    let mut e = idea_entry(sol, "Идея", json!({"origin": "agent"}));
    e.idempotency_key = Some("k-idea".into());
    let (first, created) = create_entry(&db, e.clone(), &a).await.unwrap();
    assert!(created);
    let (again, created) = create_entry(&db, e.clone(), &a).await.unwrap();
    assert!(!created);
    assert_eq!(again.id, first.id);
    e.data = json!({"origin": "agent", "priority": "high"});
    let r = create_entry(&db, e, &a).await;
    assert!(
        matches!(r, Err(StoreError::IdempotencyMismatch(_))),
        "{r:?}"
    );
}

// ---------- переходы ----------

#[test]
fn transitions_table() {
    use EntryStatus::*;
    use IdeaDecision::*;
    const TNA: &str = "transition-not-allowed";
    let h = check_idea(&json!({}), IdeaOrigin::Human).unwrap();
    let ag = check_idea(&json!({}), IdeaOrigin::Agent).unwrap();
    let r = Some("причина");
    let live = Some(TaskState::Live);
    let table: [(EntryStatus, [Result<EntryStatus, &str>; 4]); 6] = [
        // accept, defer, decline, reopen
        (New, [Ok(Accepted), Ok(Deferred), Ok(Declined), Err(TNA)]),
        (
            Proposed,
            [Ok(Accepted), Ok(Deferred), Ok(Declined), Err(TNA)],
        ),
        (
            Deferred,
            [Ok(Accepted), Ok(Deferred), Ok(Declined), Ok(New)],
        ),
        (Declined, [Err(TNA), Err(TNA), Err(TNA), Ok(New)]),
        (
            Accepted,
            [Err("already-decided"), Err(TNA), Err(TNA), Err(TNA)],
        ),
        (Done, [Err("already-decided"), Err(TNA), Err(TNA), Err(TNA)]),
    ];
    for (cur, row) in table {
        for (dec, want) in [Accept, Defer, Decline, Reopen].into_iter().zip(row) {
            assert_eq!(
                check_idea_transition(cur, &h, live, dec, r),
                want,
                "{cur:?} {dec:?}"
            );
        }
    }
    // статусы не идеи — недопустимо
    assert_eq!(check_idea_transition(Open, &h, None, Accept, r), Err(TNA));
    // reopen без причины
    for reason in [None, Some("  ")] {
        assert_eq!(
            check_idea_transition(Declined, &h, None, Reopen, reason),
            Err("reason-required")
        );
    }
    // «от агента» не начинается
    for cur in [New, Proposed, Deferred] {
        assert_eq!(
            check_idea_transition(cur, &ag, None, Accept, r),
            Err("idea-agent-origin")
        );
        assert_eq!(
            check_idea_transition(cur, &ag, None, Decline, r),
            Ok(Declined)
        );
    }
    // выход из accepted: задача снята или удалена (нет задачи — как удалена)
    for task in [Some(TaskState::Dropped), Some(TaskState::Deleted), None] {
        assert_eq!(
            check_idea_transition(Accepted, &h, task, Reopen, r),
            Ok(New)
        );
        assert_eq!(
            check_idea_transition(Accepted, &h, task, Reopen, None),
            Err("reason-required")
        );
        assert_eq!(
            check_idea_transition(Accepted, &h, task, Decline, r),
            Ok(Declined)
        );
        assert_eq!(
            check_idea_transition(Accepted, &h, task, Defer, r),
            Err(TNA)
        );
        assert_eq!(
            check_idea_transition(Accepted, &h, task, Accept, r),
            Err("already-decided")
        );
    }
    for task in [Some(TaskState::Live), Some(TaskState::Done)] {
        assert_eq!(
            check_idea_transition(Accepted, &h, task, Reopen, r),
            Err(TNA)
        );
        assert_eq!(
            check_idea_transition(Accepted, &h, task, Decline, r),
            Err(TNA)
        );
    }
}

// ---------- предложение ----------

#[tokio::test]
async fn propose_creates_question_with_fixed_options() {
    let (db, sol, _) = setup("propose").await;
    let i = new_idea(&db, sol, "Кэш выжимок на диске", "human", "high").await;
    let (idea, q) = propose_idea(
        &db,
        i.id,
        Some("Сейчас как раз правим кэш".into()),
        None,
        None,
        &agent(),
    )
    .await
    .unwrap();
    assert_eq!(idea.status, Some(EntryStatus::Proposed));
    assert_eq!(idea.rev, 2);
    let d = data_of(&idea);
    assert_eq!(d.question_id, Some(q.id));
    assert!(d.proposed_at.is_some());
    assert_eq!(q.kind, EntryKind::Question);
    assert_eq!(q.status, Some(EntryStatus::Open));
    assert_eq!(q.space_id, sol);
    assert_eq!(q.title, "Начать идею «Кэш выжимок на диске»?");
    assert!(q.body.contains("Сейчас как раз правим кэш"), "{}", q.body);
    assert!(q.body.contains(&format!("w:{}", i.id)), "{}", q.body);
    assert_eq!(
        (q.tags.clone(), q.area.clone()),
        (i.tags.clone(), i.area.clone())
    );
    let qd = QuestionData::from_stored(&q.data);
    let opts: Vec<(&str, &str)> = qd
        .options
        .iter()
        .map(|o| (o.key.as_str(), o.text.as_str()))
        .collect();
    assert_eq!(opts, IDEA_OPTIONS);
    assert_eq!(qd.recommendation, None);
    assert_eq!(qd.idea, Some(i.id));
    assert_eq!(qd.task_space, Some(sol));
    let edges = count_id(
        &db,
        "SELECT count(*) FROM edges WHERE src_id = ?1 AND type = 'relates_to'
           AND origin = 'system' AND dst_kind = 'entry' AND deleted_at IS NULL",
        q.id,
    )
    .await;
    assert_eq!(edges, 1);
    // клиент не задаёт серверные поля вопроса
    let mut fake = idea_entry(sol, "Вопрос", json!({ "idea": i.id.to_string() }));
    fake.kind = EntryKind::Question;
    let r = create_entry(&db, fake, &agent()).await;
    assert!(
        matches!(&r, Err(StoreError::BadInput(m)) if m.contains("поле ведёт сервер")),
        "{r:?}"
    );
    // длинный заголовок усекается до предела
    let long = new_idea(&db, sol, &"я".repeat(500), "human", "normal").await;
    let (_, q) = propose_idea(&db, long.id, None, None, None, &agent())
        .await
        .unwrap();
    assert!(q.title.chars().count() <= zam_store::TITLE_MAX_CHARS);
    assert!(q.title.starts_with("Начать идею «"));
    // text длиннее предела
    let other = new_idea(&db, sol, "Ещё идея", "human", "normal").await;
    let r = propose_idea(&db, other.id, Some("т".repeat(2001)), None, None, &agent()).await;
    assert!(
        matches!(r, Err(IdeaError::Store(StoreError::BadInput(_)))),
        "{r:?}"
    );
}

#[tokio::test]
async fn propose_twice_returns_same_question() {
    let (db, sol, _) = setup("propose-twice").await;
    let i = new_idea(&db, sol, "Идея", "human", "normal").await;
    let (a, q1) = propose_idea(&db, i.id, None, None, None, &agent())
        .await
        .unwrap();
    let (b, q2) = propose_idea(&db, i.id, Some("снова".into()), None, None, &agent())
        .await
        .unwrap();
    assert_eq!(q1.id, q2.id);
    assert_eq!(a.rev, b.rev);
    assert_eq!(
        count(&db, "SELECT count(*) FROM entries WHERE kind = 'question'").await,
        1
    );
}

#[tokio::test]
async fn propose_agent_origin_conflict() {
    let (db, sol, _) = setup("propose-agent").await;
    let i = new_idea(&db, sol, "Своя идея агента", "agent", "normal").await;
    let r = propose_idea(&db, i.id, None, None, None, &agent()).await;
    assert!(matches!(r, Err(IdeaError::AgentOrigin)), "{r:?}");
    let r = decide_idea(&db, i.id, nd(IdeaDecision::Accept), &human()).await;
    assert!(matches!(r, Err(IdeaError::AgentOrigin)), "{r:?}");
    // отклонённая не предлагается
    let h = new_idea(&db, sol, "Идея человека", "human", "normal").await;
    decide_idea(&db, h.id, nd(IdeaDecision::Decline), &human())
        .await
        .unwrap();
    let r = propose_idea(&db, h.id, None, None, None, &agent()).await;
    assert!(
        matches!(
            r,
            Err(IdeaError::Transition {
                from: EntryStatus::Declined,
                ..
            })
        ),
        "{r:?}"
    );
    // не идея
    let mut t = idea_entry(sol, "Задача", json!({}));
    t.kind = EntryKind::Task;
    let (t, _) = create_entry(&db, t, &agent()).await.unwrap();
    let r = propose_idea(&db, t.id, None, None, None, &agent()).await;
    assert!(matches!(r, Err(IdeaError::NotIdea)), "{r:?}");
    assert_eq!(
        count(&db, "SELECT count(*) FROM entries WHERE kind = 'question'").await,
        0
    );
}

#[tokio::test]
async fn propose_knowledge_idea_task_space_from_solution() {
    let (db, sol, kn) = setup("propose-kn").await;
    let i = new_idea(&db, kn, "Общая идея", "human", "normal").await;
    let (_, q) = propose_idea(&db, i.id, None, None, Some(sol), &agent())
        .await
        .unwrap();
    assert_eq!(QuestionData::from_stored(&q.data).task_space, Some(sol));
    assert_eq!(q.space_id, kn);
    // без решения запроса — пространство идеи
    let j = new_idea(&db, kn, "Вторая общая", "human", "normal").await;
    let (_, q) = propose_idea(&db, j.id, None, None, None, &agent())
        .await
        .unwrap();
    assert_eq!(QuestionData::from_stored(&q.data).task_space, Some(kn));
    // явное пространство чужой базы — TaskSpace
    let k = new_idea(&db, kn, "Третья общая", "human", "normal").await;
    let r = propose_idea(&db, k.id, None, Some(Id::new()), None, &agent()).await;
    assert!(matches!(r, Err(IdeaError::TaskSpace(_))), "{r:?}");
    assert_eq!(
        get_entry(&db, k.id).await.unwrap().status,
        Some(EntryStatus::New)
    );
}

// ---------- «начать» и ответ ----------

#[tokio::test]
async fn accept_creates_task_edge_and_answers_question() {
    let (db, sol, _) = setup("accept").await;
    let mut e = idea_entry(
        sol,
        "Кэш на диске",
        json!({"origin": "human", "links": ["https://example.org/x"]}),
    );
    e.body = "Замысел: держать кэш на диске.".into();
    let (i, _) = create_entry(&db, e, &human()).await.unwrap();
    let (_, q) = propose_idea(&db, i.id, None, None, None, &agent())
        .await
        .unwrap();
    let dec = decide_idea(&db, i.id, nd(IdeaDecision::Accept), &human())
        .await
        .unwrap();
    assert_eq!(dec.idea.status, Some(EntryStatus::Accepted));
    let task = dec.task.clone().unwrap();
    let d = data_of(&dec.idea);
    assert_eq!(d.task_id, Some(task.id));
    let decided = d.decided.unwrap();
    assert_eq!(
        (decided.decision, decided.by, decided.channel),
        (IdeaDecision::Accept, AnswerBy::Human, Channel::Webui)
    );
    assert_eq!(task.kind, EntryKind::Task);
    assert_eq!(task.status, Some(EntryStatus::Open));
    assert_eq!(task.space_id, sol);
    assert_eq!(task.title, i.title);
    assert_eq!(
        (task.tags.clone(), task.area.clone()),
        (i.tags.clone(), i.area.clone())
    );
    assert!(
        task.body.contains("Замысел: держать кэш на диске."),
        "{}",
        task.body
    );
    assert!(
        task.body.contains(&format!("Идея: w:{}", i.id)),
        "{}",
        task.body
    );
    assert_eq!(
        TaskData::from_stored(&task.data).links,
        vec!["https://example.org/x"]
    );
    let edges = count_id(
        &db,
        "SELECT count(*) FROM edges WHERE src_id = ?1 AND type = 'derived_from'
           AND origin = 'system' AND deleted_at IS NULL",
        task.id,
    )
    .await;
    assert_eq!(edges, 1);
    let dst: Id = db
        .read(move |c| {
            Ok(c.query_row(
                "SELECT dst_id FROM edges WHERE src_id = ?1 AND type = 'derived_from'",
                [task.id],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(dst, i.id);
    let question = dec.question.unwrap();
    assert_eq!(question.id, q.id);
    assert_eq!(question.status, Some(EntryStatus::Answered));
    let ans = QuestionData::from_stored(&question.data).answer.unwrap();
    assert_eq!(
        (ans.option.as_deref(), ans.by),
        (Some("accept"), AnswerBy::Human)
    );
    // второй «начать» — та же задача, новой нет
    let r = decide_idea(&db, i.id, nd(IdeaDecision::Accept), &human()).await;
    assert!(
        matches!(r, Err(IdeaError::AlreadyDecided { task_id: Some(t) }) if t == dec.task.as_ref().unwrap().id),
        "{r:?}"
    );
    assert_eq!(
        count(&db, "SELECT count(*) FROM entries WHERE kind = 'task'").await,
        1
    );
}

#[tokio::test]
async fn exactly_one_decision_per_path() {
    let (db, sol, _) = setup("one-decision").await;
    // ответ на вопрос-предложение
    let a = new_idea(&db, sol, "Идея А", "human", "normal").await;
    let (_, qa) = propose_idea(&db, a.id, None, None, None, &agent())
        .await
        .unwrap();
    let Answered {
        question,
        decision,
        idea,
        task,
    } = answer_question(
        &db,
        qa.id,
        answer(Some("accept"), None, AnswerBy::Human),
        &agent(),
    )
    .await
    .unwrap();
    assert_eq!(question.status, Some(EntryStatus::Answered));
    assert_eq!(decision.kind, EntryKind::Insight);
    let idea = idea.unwrap();
    assert_eq!(idea.status, Some(EntryStatus::Accepted));
    assert_eq!(data_of(&idea).task_id, task.map(|t| t.id));
    assert_eq!(count(&db, DECISIONS).await, 1);
    // повторный ответ — already-answered
    let r = answer_question(
        &db,
        qa.id,
        answer(Some("accept"), None, AnswerBy::Human),
        &agent(),
    )
    .await;
    assert!(
        matches!(r, Err(QuestionError::AlreadyAnswered { .. })),
        "{r:?}"
    );
    // decide при открытом вопросе
    let b = new_idea(&db, sol, "Идея Б", "human", "normal").await;
    let (_, qb) = propose_idea(&db, b.id, None, None, None, &agent())
        .await
        .unwrap();
    decide_idea(&db, b.id, nd(IdeaDecision::Decline), &human())
        .await
        .unwrap();
    assert_eq!(count(&db, DECISIONS).await, 2);
    let qb = get_entry(&db, qb.id).await.unwrap();
    assert_eq!(
        QuestionData::from_stored(&qb.data)
            .answer
            .unwrap()
            .option
            .as_deref(),
        Some("decline")
    );
    let r = answer_question(
        &db,
        qb.id,
        answer(Some("accept"), None, AnswerBy::Human),
        &agent(),
    )
    .await;
    assert!(
        matches!(r, Err(QuestionError::AlreadyAnswered { .. })),
        "{r:?}"
    );
    // decide без вопроса — решения нет
    let c = new_idea(&db, sol, "Идея В", "human", "normal").await;
    let dec = decide_idea(&db, c.id, nd(IdeaDecision::Accept), &human())
        .await
        .unwrap();
    assert!(dec.question.is_none());
    assert_eq!(count(&db, DECISIONS).await, 2);
}

const FAIL_TASK: &str = "CREATE TRIGGER test_fail_task BEFORE INSERT ON entries
    WHEN NEW.kind = 'task' BEGIN SELECT RAISE(ABORT, 'test-fail-task'); END";

#[tokio::test]
async fn accept_rolls_back_when_task_insert_fails() {
    let (db, sol, _) = setup("rollback").await;
    let i = new_idea(&db, sol, "Идея", "human", "normal").await;
    let (i, q) = propose_idea(&db, i.id, None, None, None, &agent())
        .await
        .unwrap();
    let edges = count(&db, "SELECT count(*) FROM edges").await;
    let revs = count(&db, "SELECT count(*) FROM entry_revisions").await;
    exec(&db, FAIL_TASK.into()).await.unwrap();
    let r = decide_idea(&db, i.id, nd(IdeaDecision::Accept), &human()).await;
    assert!(
        matches!(&r, Err(IdeaError::Store(StoreError::Sqlite(e))) if e.to_string().contains("test-fail-task")),
        "{r:?}"
    );
    assert_eq!(snap(&get_entry(&db, i.id).await.unwrap()), snap(&i));
    assert_eq!(snap(&get_entry(&db, q.id).await.unwrap()), snap(&q));
    assert_eq!(count(&db, "SELECT count(*) FROM edges").await, edges);
    assert_eq!(
        count(&db, "SELECT count(*) FROM entry_revisions").await,
        revs
    );
    assert_eq!(count(&db, DECISIONS).await, 0);
    assert_eq!(
        count(&db, "SELECT count(*) FROM entries WHERE kind = 'task'").await,
        0
    );
}

#[tokio::test]
async fn answer_accept_on_proposal_is_one_tx() {
    let (db, sol, _) = setup("answer-tx").await;
    let i = new_idea(&db, sol, "Идея", "human", "normal").await;
    let (i, q) = propose_idea(&db, i.id, None, None, None, &agent())
        .await
        .unwrap();
    let edges = count(&db, "SELECT count(*) FROM edges").await;
    exec(&db, FAIL_TASK.into()).await.unwrap();
    let r = answer_question(
        &db,
        q.id,
        answer(Some("accept"), None, AnswerBy::Human),
        &agent(),
    )
    .await;
    assert!(
        matches!(&r, Err(QuestionError::Store(StoreError::Sqlite(e))) if e.to_string().contains("test-fail-task")),
        "{r:?}"
    );
    assert_eq!(snap(&get_entry(&db, i.id).await.unwrap()), snap(&i));
    assert_eq!(snap(&get_entry(&db, q.id).await.unwrap()), snap(&q));
    assert_eq!(count(&db, DECISIONS).await, 0);
    assert_eq!(count(&db, "SELECT count(*) FROM edges").await, edges);
    exec(&db, "DROP TRIGGER test_fail_task".into())
        .await
        .unwrap();
    // логическая ошибка перехода после ответа тоже откатывает ответ: пространство задачи
    // в вопросе — не этой базы
    exec(
        &db,
        format!(
            "UPDATE entries SET data = json_set(data, '$.task_space', '{}') WHERE id = '{}'",
            Id::new(),
            q.id
        ),
    )
    .await
    .unwrap();
    let q = get_entry(&db, q.id).await.unwrap();
    let r = answer_question(
        &db,
        q.id,
        answer(Some("accept"), None, AnswerBy::Human),
        &agent(),
    )
    .await;
    assert!(
        matches!(r, Err(QuestionError::Idea(IdeaError::TaskSpace(_)))),
        "{r:?}"
    );
    assert_eq!(snap(&get_entry(&db, q.id).await.unwrap()), snap(&q));
    assert_eq!(snap(&get_entry(&db, i.id).await.unwrap()), snap(&i));
    assert_eq!(count(&db, DECISIONS).await, 0);
}

#[tokio::test]
async fn answer_proposal_requires_option() {
    let (db, sol, _) = setup("answer-option").await;
    let i = new_idea(&db, sol, "Идея", "human", "normal").await;
    let (_, q) = propose_idea(&db, i.id, None, None, None, &agent())
        .await
        .unwrap();
    let r = answer_question(
        &db,
        q.id,
        answer(None, Some("давай"), AnswerBy::Human),
        &agent(),
    )
    .await;
    assert!(matches!(r, Err(QuestionError::OptionRequired)), "{r:?}");
    let r = answer_question(
        &db,
        q.id,
        answer(Some("maybe"), None, AnswerBy::Human),
        &agent(),
    )
    .await;
    assert!(matches!(r, Err(QuestionError::UnknownOption(_))), "{r:?}");
    assert_eq!(
        get_entry(&db, q.id).await.unwrap().status,
        Some(EntryStatus::Open)
    );
    // текст — комментарий к варианту
    let ok = answer_question(
        &db,
        q.id,
        answer(Some("decline"), Some("не сейчас"), AnswerBy::Human),
        &agent(),
    )
    .await
    .unwrap();
    assert_eq!(ok.idea.unwrap().status, Some(EntryStatus::Declined));
    assert!(ok.task.is_none());
}

#[tokio::test]
async fn answer_proposal_by_agent_rejected_before_idea_read() {
    let (db, sol, _) = setup("answer-agent").await;
    let i = new_idea(&db, sol, "Идея", "human", "normal").await;
    let (_, q) = propose_idea(&db, i.id, None, None, None, &agent())
        .await
        .unwrap();
    let r = answer_question(
        &db,
        q.id,
        answer(Some("accept"), None, AnswerBy::Agent),
        &agent(),
    )
    .await;
    assert!(matches!(r, Err(QuestionError::ByAgentOnProposal)), "{r:?}");
    // идея по data.idea не читается: несуществующая идея — тот же отказ
    exec(
        &db,
        format!(
            "UPDATE entries SET data = json_set(data, '$.idea', '{}') WHERE id = '{}'",
            Id::new(),
            q.id
        ),
    )
    .await
    .unwrap();
    let r = answer_question(
        &db,
        q.id,
        answer(Some("accept"), None, AnswerBy::Agent),
        &agent(),
    )
    .await;
    assert!(matches!(r, Err(QuestionError::ByAgentOnProposal)), "{r:?}");
    assert_eq!(
        get_entry(&db, q.id).await.unwrap().status,
        Some(EntryStatus::Open)
    );
    assert_eq!(count(&db, DECISIONS).await, 0);
}

#[tokio::test]
async fn answer_on_deleted_idea_conflict() {
    let (db, sol, _) = setup("answer-deleted").await;
    let i = new_idea(&db, sol, "Идея", "human", "normal").await;
    let (_, q) = propose_idea(&db, i.id, None, None, None, &agent())
        .await
        .unwrap();
    // идея в корзине, вопрос остался открытым (в обход §5.6 — прямым SQL)
    exec(
        &db,
        format!(
            "UPDATE entries SET deleted_at = '2026-11-04T00:00:00Z' WHERE id = '{}'",
            i.id
        ),
    )
    .await
    .unwrap();
    let r = answer_question(
        &db,
        q.id,
        answer(Some("accept"), None, AnswerBy::Human),
        &agent(),
    )
    .await;
    assert!(
        matches!(r, Err(QuestionError::Idea(IdeaError::IdeaDeleted))),
        "{r:?}"
    );
    assert_eq!(
        get_entry(&db, q.id).await.unwrap().status,
        Some(EntryStatus::Open)
    );
    assert_eq!(count(&db, DECISIONS).await, 0);
}

#[tokio::test]
async fn answer_horizon_only_for_proposal_defer() {
    let (db, sol, _) = setup("answer-horizon").await;
    let mut plain = idea_entry(
        sol,
        "Какую БД?",
        json!({"options": [{"key": "a", "text": "SQLite"}, {"key": "b", "text": "Postgres"}]}),
    );
    plain.kind = EntryKind::Question;
    let (plain, _) = create_entry(&db, plain, &agent()).await.unwrap();
    let mut a = answer(Some("a"), None, AnswerBy::Human);
    a.horizon = Some(today() + days(10));
    let r = answer_question(&db, plain.id, a.clone(), &agent()).await;
    assert!(matches!(r, Err(QuestionError::HorizonNotAllowed)), "{r:?}");
    let i = new_idea(&db, sol, "Идея", "human", "normal").await;
    let (_, q) = propose_idea(&db, i.id, None, None, None, &agent())
        .await
        .unwrap();
    a.option = Some("accept".into());
    let r = answer_question(&db, q.id, a.clone(), &agent()).await;
    assert!(matches!(r, Err(QuestionError::HorizonNotAllowed)), "{r:?}");
    a.option = Some("defer".into());
    let ok = answer_question(&db, q.id, a, &agent()).await.unwrap();
    let idea = ok.idea.unwrap();
    assert_eq!(idea.status, Some(EntryStatus::Deferred));
    assert_eq!(data_of(&idea).horizon, Some(today() + days(10)));
}

// ---------- отсрочка и возврат ----------

#[tokio::test]
async fn defer_default_horizon_past_rejected_and_repeat() {
    let (db, sol, _) = setup("defer").await;
    let i = new_idea(&db, sol, "Идея", "human", "normal").await;
    let dec = decide_idea(&db, i.id, nd(IdeaDecision::Defer), &human())
        .await
        .unwrap();
    assert_eq!(dec.idea.status, Some(EntryStatus::Deferred));
    assert_eq!(data_of(&dec.idea).horizon, Some(today() + days(30)));
    for bad in [today() - days(1), today()] {
        let mut d = nd(IdeaDecision::Defer);
        d.horizon = Some(bad);
        let r = decide_idea(&db, i.id, d, &human()).await;
        assert!(matches!(r, Err(IdeaError::BadHorizon(_))), "{bad}: {r:?}");
    }
    let mut d = nd(IdeaDecision::Defer);
    d.horizon = Some(today() + days(5));
    let again = decide_idea(&db, i.id, d, &human()).await.unwrap();
    assert_eq!(again.idea.status, Some(EntryStatus::Deferred));
    assert_eq!(data_of(&again.idea).horizon, Some(today() + days(5)));
    // horizon — только у defer
    let mut d = nd(IdeaDecision::Decline);
    d.horizon = Some(today() + days(5));
    let r = decide_idea(&db, i.id, d, &human()).await;
    assert!(matches!(r, Err(IdeaError::BadHorizon(_))), "{r:?}");
    // reopen снимает horizon
    let mut d = nd(IdeaDecision::Reopen);
    d.reason = Some("вернёмся".into());
    let back = decide_idea(&db, i.id, d, &human()).await.unwrap();
    assert_eq!(back.idea.status, Some(EntryStatus::New));
    assert_eq!(data_of(&back.idea).horizon, None);
}

#[tokio::test]
async fn obsolete_proposal_returns_idea_to_new() {
    let (db, sol, _) = setup("obsolete").await;
    let i = new_idea(&db, sol, "Идея", "human", "normal").await;
    let (_, q) = propose_idea(&db, i.id, None, None, None, &agent())
        .await
        .unwrap();
    update_entry(
        &db,
        q.id,
        None,
        EntryPatch {
            status: Some(EntryStatus::Obsolete),
            reason: Some("неактуально".into()),
            ..EntryPatch::default()
        },
        &agent(),
    )
    .await
    .unwrap();
    let idea = get_entry(&db, i.id).await.unwrap();
    assert_eq!(idea.status, Some(EntryStatus::New));
    assert_eq!(data_of(&idea).question_id, None);
    // снова предлагается — новый вопрос
    let (_, q2) = propose_idea(&db, i.id, None, None, None, &agent())
        .await
        .unwrap();
    assert_ne!(q2.id, q.id);
}

#[tokio::test]
async fn reopen_from_accepted_after_task_dropped_then_new_task() {
    let (db, sol, _) = setup("reopen").await;
    let i = new_idea(&db, sol, "Идея", "human", "normal").await;
    let t1 = decide_idea(&db, i.id, nd(IdeaDecision::Accept), &human())
        .await
        .unwrap()
        .task
        .unwrap();
    let mut reopen = nd(IdeaDecision::Reopen);
    reopen.reason = Some("задача снята".into());
    let r = decide_idea(&db, i.id, reopen.clone(), &human()).await;
    assert!(
        matches!(
            r,
            Err(IdeaError::Transition {
                from: EntryStatus::Accepted,
                ..
            })
        ),
        "{r:?}"
    );
    update_entry(
        &db,
        t1.id,
        None,
        EntryPatch {
            status: Some(EntryStatus::Dropped),
            ..EntryPatch::default()
        },
        &agent(),
    )
    .await
    .unwrap();
    assert_eq!(
        get_entry(&db, i.id).await.unwrap().status,
        Some(EntryStatus::Accepted)
    );
    let r = decide_idea(&db, i.id, nd(IdeaDecision::Reopen), &human()).await;
    assert!(matches!(r, Err(IdeaError::ReasonRequired)), "{r:?}");
    let back = decide_idea(&db, i.id, reopen, &human()).await.unwrap();
    assert_eq!(back.idea.status, Some(EntryStatus::New));
    let d = data_of(&back.idea);
    assert_eq!((d.task_id, d.prev_task_ids.clone()), (None, vec![t1.id]));
    let t2 = decide_idea(&db, i.id, nd(IdeaDecision::Accept), &human())
        .await
        .unwrap();
    let t2_id = t2.task.unwrap().id;
    assert_ne!(t2_id, t1.id);
    let d = data_of(&t2.idea);
    assert_eq!((d.task_id, d.prev_task_ids), (Some(t2_id), vec![t1.id]));
    // задача удалена — отказаться можно
    let j = new_idea(&db, sol, "Другая", "human", "normal").await;
    let tj = decide_idea(&db, j.id, nd(IdeaDecision::Accept), &human())
        .await
        .unwrap()
        .task
        .unwrap();
    delete_entry(&db, tj.id, None, &agent()).await.unwrap();
    assert_eq!(
        get_entry(&db, j.id).await.unwrap().status,
        Some(EntryStatus::Accepted)
    );
    let dec = decide_idea(&db, j.id, nd(IdeaDecision::Decline), &human())
        .await
        .unwrap();
    assert_eq!(dec.idea.status, Some(EntryStatus::Declined));
}

// ---------- принятие и задача ----------

#[tokio::test]
async fn adopt_sets_origin_human_once() {
    let (db, sol, _) = setup("adopt").await;
    let i = new_idea(&db, sol, "Мысль агента", "agent", "normal").await;
    assert_eq!(i.author.kind, AuthorKind::Agent);
    let user = Id::new();
    let e = adopt_idea(&db, i.id, user, &human()).await.unwrap();
    let d = data_of(&e);
    assert_eq!(d.origin, IdeaOrigin::Human);
    assert_eq!(d.adopted_by, Some(user));
    assert!(d.adopted_at.is_some());
    assert_eq!(e.author.kind, AuthorKind::Agent, "автор записи прежний");
    assert_eq!(e.rev, i.rev + 1);
    let r = adopt_idea(&db, i.id, user, &human()).await;
    assert!(matches!(r, Err(IdeaError::AlreadyAdopted)), "{r:?}");
    propose_idea(&db, i.id, None, None, None, &agent())
        .await
        .unwrap();
}

#[tokio::test]
async fn task_done_marks_idea_done_reopen_back() {
    let (db, sol, _) = setup("task-done").await;
    let i = new_idea(&db, sol, "Идея", "human", "normal").await;
    let t = decide_idea(&db, i.id, nd(IdeaDecision::Accept), &human())
        .await
        .unwrap()
        .task
        .unwrap();
    let set = |s: EntryStatus, reason: Option<&str>| {
        let db = db.clone();
        let reason = reason.map(str::to_string);
        async move {
            update_entry(
                &db,
                t.id,
                None,
                EntryPatch {
                    status: Some(s),
                    reason,
                    ..EntryPatch::default()
                },
                &agent(),
            )
            .await
            .unwrap();
        }
    };
    set(EntryStatus::InProgress, None).await;
    assert_eq!(
        get_entry(&db, i.id).await.unwrap().status,
        Some(EntryStatus::Accepted)
    );
    set(EntryStatus::Done, None).await;
    let idea = get_entry(&db, i.id).await.unwrap();
    assert_eq!(idea.status, Some(EntryStatus::Done));
    assert!(data_of(&idea).done_at.is_some());
    set(EntryStatus::Open, Some("нашли баг")).await;
    let idea = get_entry(&db, i.id).await.unwrap();
    assert_eq!(idea.status, Some(EntryStatus::Accepted));
    assert!(data_of(&idea).done_at.is_none());
    set(EntryStatus::Dropped, None).await;
    assert_eq!(
        get_entry(&db, i.id).await.unwrap().status,
        Some(EntryStatus::Accepted)
    );
}

// ---------- корзина ----------

#[tokio::test]
async fn delete_idea_obsoletes_question() {
    let (db, sol, _) = setup("delete-idea").await;
    let i = new_idea(&db, sol, "Идея", "human", "normal").await;
    let (_, q) = propose_idea(&db, i.id, None, None, None, &agent())
        .await
        .unwrap();
    delete_entry(&db, i.id, None, &human()).await.unwrap();
    let q = get_entry(&db, q.id).await.unwrap();
    assert_eq!(q.status, Some(EntryStatus::Obsolete));
    assert!(q.deleted_at.is_none());
}

#[tokio::test]
async fn delete_question_returns_idea_to_new() {
    let (db, sol, _) = setup("delete-question").await;
    let i = new_idea(&db, sol, "Идея", "human", "normal").await;
    let (_, q) = propose_idea(&db, i.id, None, None, None, &agent())
        .await
        .unwrap();
    delete_entry(&db, q.id, None, &human()).await.unwrap();
    let idea = get_entry(&db, i.id).await.unwrap();
    assert_eq!(idea.status, Some(EntryStatus::New));
    assert_eq!(data_of(&idea).question_id, None);
}

// ---------- пространства и связи ----------

#[tokio::test]
async fn knowledge_idea_task_into_solution_same_db() {
    let (db, sol, kn) = setup("kn-task").await;
    let (sol2, _) = create_solution(&db, "g-kn-task-2", "Второе").await.unwrap();
    let i = new_idea(&db, kn, "Общая идея", "human", "normal").await;
    let mut d = nd(IdeaDecision::Accept);
    d.task_space = Some(Id::new());
    let r = decide_idea(&db, i.id, d, &human()).await;
    assert!(matches!(r, Err(IdeaError::TaskSpace(_))), "{r:?}");
    let mut d = nd(IdeaDecision::Accept);
    d.task_space = Some(sol);
    let dec = decide_idea(&db, i.id, d, &human()).await.unwrap();
    let task = dec.task.unwrap();
    assert_eq!(task.space_id, sol);
    // решение запроса той же базы — по умолчанию для идеи Knowledge
    let j = new_idea(&db, kn, "Вторая общая", "human", "normal").await;
    let mut d = nd(IdeaDecision::Accept);
    d.request_solution = Some(sol2.id);
    assert_eq!(
        decide_idea(&db, j.id, d, &human())
            .await
            .unwrap()
            .task
            .unwrap()
            .space_id,
        sol2.id
    );
    // идея решения — только своё пространство
    let s = new_idea(&db, sol, "Идея решения", "human", "normal").await;
    for other in [kn, sol2.id] {
        let mut d = nd(IdeaDecision::Accept);
        d.task_space = Some(other);
        let r = decide_idea(&db, s.id, d, &human()).await;
        assert!(matches!(r, Err(IdeaError::TaskSpace(_))), "{r:?}");
    }
    // чужое решение запроса для идеи решения не подходит — задача в пространстве идеи
    let mut d = nd(IdeaDecision::Accept);
    d.request_solution = Some(sol2.id);
    assert_eq!(
        decide_idea(&db, s.id, d, &human())
            .await
            .unwrap()
            .task
            .unwrap()
            .space_id,
        sol
    );
    // Rules/Human — вне правил
    let rules = list_spaces(&db)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.kind == SpaceKind::Rules)
        .unwrap()
        .id;
    let k = new_idea(&db, kn, "Третья общая", "human", "normal").await;
    let mut d = nd(IdeaDecision::Accept);
    d.task_space = Some(rules);
    let r = decide_idea(&db, k.id, d, &human()).await;
    assert!(matches!(r, Err(IdeaError::TaskSpace(_))), "{r:?}");
}

#[tokio::test]
async fn client_derived_from_to_idea_rejected() {
    let (db, sol, _) = setup("client-edge").await;
    let i = new_idea(&db, sol, "Идея", "human", "normal").await;
    let mut t = idea_entry(sol, "Задача", json!({}));
    t.kind = EntryKind::Task;
    t.links = vec![(EdgeType::DerivedFrom, entry_ref(i.id))];
    let r = create_entry(&db, t.clone(), &agent()).await;
    assert!(matches!(&r, Err(StoreError::IdeaLinkDenied)), "{r:?}");
    assert!(r.unwrap_err().to_string().contains("«начать»"));
    t.links = vec![(EdgeType::RelatesTo, entry_ref(i.id))];
    let (task, _) = create_entry(&db, t, &agent()).await.unwrap();
    let r = db
        .add_edge(
            NewEdge {
                src: entry_ref(task.id),
                dst: entry_ref(i.id),
                kind: EdgeType::DerivedFrom,
            },
            &agent(),
        )
        .await;
    assert!(matches!(&r, Err(StoreError::IdeaLinkDenied)), "{r:?}");
}

/// Ревью задачи 2: права «кто решает» сервер проверяет по `origin` до транзакции; ожидаемое
/// происхождение сверяется внутри неё — расхождение (идею приняли как свою между чтением и
/// записью) — `OriginChanged` без записи.
#[tokio::test]
async fn decide_checks_expected_origin_in_tx() {
    let (db, sol, _) = setup("expected-origin").await;
    let i = new_idea(&db, sol, "Чья идея", "human", "normal").await;
    let d = NewDecision {
        by: AnswerBy::Agent,
        channel: Channel::Chat,
        expected_origin: Some(IdeaOrigin::Agent),
        ..nd(IdeaDecision::Decline)
    };
    let r = decide_idea(&db, i.id, d, &agent()).await;
    assert!(matches!(r, Err(IdeaError::OriginChanged)), "{r:?}");
    let e = get_entry(&db, i.id).await.unwrap();
    assert_eq!((e.rev, e.status), (i.rev, Some(EntryStatus::New)));
    let d = NewDecision {
        expected_origin: Some(IdeaOrigin::Human),
        ..nd(IdeaDecision::Decline)
    };
    let r = decide_idea(&db, i.id, d, &human()).await.unwrap();
    assert_eq!(r.idea.status, Some(EntryStatus::Declined));
}

// ---------- отбор и лента ----------

#[tokio::test]
async fn proactive_ideas_selection() {
    let (db, sol, kn) = setup("proactive").await;
    let high = new_idea(&db, sol, "Высокая", "human", "high").await;
    let norm1 = new_idea(&db, sol, "Обычная 1", "human", "normal").await;
    let norm2 = new_idea(&db, sol, "Обычная 2", "human", "normal").await;
    let low = new_idea(&db, sol, "Низкая", "human", "low").await;
    let agent_idea = new_idea(&db, sol, "От агента", "agent", "high").await;
    let proposed = new_idea(&db, sol, "Предложенная", "human", "high").await;
    propose_idea(&db, proposed.id, None, None, None, &agent())
        .await
        .unwrap();
    let declined = new_idea(&db, sol, "Отклонённая", "human", "high").await;
    decide_idea(&db, declined.id, nd(IdeaDecision::Decline), &human())
        .await
        .unwrap();
    let future = new_idea(&db, sol, "Отложенная надолго", "human", "high").await;
    let mut d = nd(IdeaDecision::Defer);
    d.horizon = Some(today() + days(1));
    decide_idea(&db, future.id, d, &human()).await.unwrap();
    let past = new_idea(&db, sol, "Отложенная, срок прошёл", "human", "normal").await;
    decide_idea(&db, past.id, nd(IdeaDecision::Defer), &human())
        .await
        .unwrap();
    let seen_idea = new_idea(&db, sol, "Уже показана", "human", "high").await;
    let other_space = new_idea(&db, kn, "Общая", "human", "high").await;
    let accepted = new_idea(&db, sol, "Начатая", "human", "high").await;
    decide_idea(&db, accepted.id, nd(IdeaDecision::Accept), &human())
        .await
        .unwrap();
    let done = new_idea(&db, sol, "Сделанная", "human", "high").await;
    let done_task = decide_idea(&db, done.id, nd(IdeaDecision::Accept), &human())
        .await
        .unwrap()
        .task
        .unwrap();
    update_entry(
        &db,
        done_task.id,
        None,
        EntryPatch {
            status: Some(EntryStatus::Done),
            ..EntryPatch::default()
        },
        &agent(),
    )
    .await
    .unwrap();
    assert_eq!(
        get_entry(&db, done.id).await.unwrap().status,
        Some(EntryStatus::Done)
    );
    // наступивший horizon и порядок по updated_at — прямым SQL
    let yesterday = (today() - days(1)).to_string();
    exec(
        &db,
        format!(
            "UPDATE entries SET data = json_set(data, '$.horizon', '{yesterday}') WHERE id = '{}'",
            past.id
        ),
    )
    .await
    .unwrap();
    for (id, ts) in [
        (norm1.id, "2026-01-01T00:00:00Z"),
        (norm2.id, "2026-01-02T00:00:00Z"),
        (past.id, "2026-01-03T00:00:00Z"),
    ] {
        exec(
            &db,
            format!("UPDATE entries SET updated_at = '{ts}' WHERE id = '{id}'"),
        )
        .await
        .unwrap();
    }
    let seen: HashSet<Id> = [seen_idea.id].into_iter().collect();
    let got = proactive_ideas(&db, &[sol], today(), &seen, 10)
        .await
        .unwrap();
    let ids: Vec<Id> = got.iter().map(|e| e.id).collect();
    assert_eq!(ids, vec![high.id, past.id, norm2.id, norm1.id, low.id]);
    let three = proactive_ideas(&db, &[sol], today(), &seen, 3)
        .await
        .unwrap();
    assert_eq!(
        three.iter().map(|e| e.id).collect::<Vec<_>>(),
        vec![high.id, past.id, norm2.id]
    );
    // предикат кандидата тот же
    for id in [
        high.id,
        norm1.id,
        norm2.id,
        low.id,
        agent_idea.id,
        proposed.id,
        declined.id,
        future.id,
        past.id,
        other_space.id,
        accepted.id,
        done.id,
    ] {
        let e = get_entry(&db, id).await.unwrap();
        let expected = ids.contains(&id) || id == other_space.id;
        assert_eq!(idea_candidate(&e, today()), expected, "{}", e.title);
    }
    assert!(idea_candidate(
        &get_entry(&db, future.id).await.unwrap(),
        today() + days(1)
    ));
    assert!(
        proactive_ideas(&db, &[], today(), &seen, 3)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        proactive_ideas(&db, &[sol], today(), &seen, 0)
            .await
            .unwrap()
            .is_empty()
    );
}

fn idea_filter(space: Id) -> EntryFilter {
    EntryFilter {
        space_id: space,
        kind: Some(EntryKind::Idea),
        statuses: Vec::new(),
        certainty: Vec::new(),
        area: None,
        tag: None,
        data_kind: None,
        decided_by: None,
        since: None,
        until: None,
        cursor: None,
        limit: 50,
        with_reports: false,
        deleted_only: false,
        origin: None,
        priority: None,
    }
}

#[tokio::test]
async fn list_filter_origin_priority() {
    let (db, sol, _) = setup("list").await;
    let h_high = new_idea(&db, sol, "Человек высокая", "human", "high").await;
    let h_low = new_idea(&db, sol, "Человек низкая", "human", "low").await;
    let a_high = new_idea(&db, sol, "Агент высокая", "agent", "high").await;
    let ids = |v: Vec<Entry>| {
        let mut v: Vec<Id> = v.into_iter().map(|e| e.id).collect();
        v.sort();
        v
    };
    let sorted = |mut v: Vec<Id>| {
        v.sort();
        v
    };
    let mut f = idea_filter(sol);
    f.origin = Some(IdeaOrigin::Agent);
    assert_eq!(ids(list_entries(&db, f).await.unwrap().0), vec![a_high.id]);
    let mut f = idea_filter(sol);
    f.origin = Some(IdeaOrigin::Human);
    assert_eq!(
        ids(list_entries(&db, f).await.unwrap().0),
        sorted(vec![h_high.id, h_low.id])
    );
    let mut f = idea_filter(sol);
    f.priority = Some(IdeaPriority::High);
    assert_eq!(
        ids(list_entries(&db, f).await.unwrap().0),
        sorted(vec![h_high.id, a_high.id])
    );
    let mut f = idea_filter(sol);
    f.priority = Some(IdeaPriority::High);
    f.origin = Some(IdeaOrigin::Human);
    assert_eq!(ids(list_entries(&db, f).await.unwrap().0), vec![h_high.id]);
    let mut f = idea_filter(sol);
    f.kind = None;
    f.origin = Some(IdeaOrigin::Human);
    let r = list_entries(&db, f).await;
    assert!(matches!(r, Err(StoreError::BadInput(_))), "{r:?}");
    // правка содержимого идеи — в new и deferred; data — слияние
    let e = update_entry(
        &db,
        h_low.id,
        Some(h_low.rev),
        EntryPatch {
            data: Some(json!({"priority": "high", "links": ["x"]})),
            ..EntryPatch::default()
        },
        &human(),
    )
    .await
    .unwrap();
    let d = data_of(&e);
    assert_eq!(
        (d.priority, d.origin, d.links),
        (IdeaPriority::High, IdeaOrigin::Human, vec!["x".to_string()])
    );
    let r = update_entry(
        &db,
        h_low.id,
        None,
        EntryPatch {
            data: Some(json!({"origin": "agent"})),
            ..EntryPatch::default()
        },
        &human(),
    )
    .await;
    assert!(matches!(r, Err(StoreError::BadInput(_))), "{r:?}");
    decide_idea(&db, h_low.id, nd(IdeaDecision::Decline), &human())
        .await
        .unwrap();
    let r = update_entry(
        &db,
        h_low.id,
        None,
        EntryPatch {
            title: Some("Новый".into()),
            ..EntryPatch::default()
        },
        &human(),
    )
    .await;
    assert!(matches!(r, Err(StoreError::Conflict(_))), "{r:?}");
}

// ---------- свойства ----------

fn json_value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::from),
        any::<i64>().prop_map(Value::from),
        "[a-z0-9:-]{0,12}".prop_map(Value::from),
        Just(Value::from("2099-01-02")),
        Just(Value::from("high")),
    ];
    leaf.prop_recursive(3, 24, 4, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4).prop_map(Value::from),
            prop::collection::btree_map(
                prop_oneof![
                    Just("priority".to_string()),
                    Just("horizon".to_string()),
                    Just("links".to_string()),
                    Just("origin".to_string()),
                    Just("task_id".to_string()),
                    "[a-z_]{1,8}",
                ],
                inner,
                0..4
            )
            .prop_map(|m| Value::Object(m.into_iter().collect())),
        ]
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]
    #[test]
    fn idea_patch_never_panics(v in json_value()) {
        let mut d = check_idea(&json!({}), IdeaOrigin::Human).unwrap();
        let before = d.clone();
        if d.apply_patch(&v).is_err() {
            prop_assert_eq!(&d, &before);
        }
        let _ = check_idea(&v, IdeaOrigin::Agent);
        let stored = IdeaData::from_stored(&v);
        prop_assert_eq!(IdeaData::from_stored(&stored.to_value()), stored);
    }
}

// ---------- рецензия задачи 1 ----------

/// I1: варианты вопроса-предложения фиксирует сервер — содержимое такого вопроса не правится;
/// устаревание (статус с причиной) по-прежнему разрешено.
#[tokio::test]
async fn proposal_question_content_locked() {
    let (db, sol, _) = setup("proposal-locked").await;
    let i = new_idea(&db, sol, "Идея", "human", "normal").await;
    let (_, q) = propose_idea(&db, i.id, None, None, None, &agent())
        .await
        .unwrap();
    for p in [
        EntryPatch {
            data: Some(json!({"options": [{"key": "accept", "text": "отказаться"}]})),
            ..EntryPatch::default()
        },
        EntryPatch {
            data: Some(json!({"recommendation": "accept"})),
            ..EntryPatch::default()
        },
        EntryPatch {
            title: Some("Начать сразу?".into()),
            ..EntryPatch::default()
        },
        EntryPatch {
            append_body: Some("ещё".into()),
            ..EntryPatch::default()
        },
    ] {
        let r = update_entry(&db, q.id, None, p, &agent()).await;
        assert!(
            matches!(&r, Err(StoreError::Conflict(m)) if m.contains("предложени")),
            "{r:?}"
        );
    }
    assert_eq!(snap(&get_entry(&db, q.id).await.unwrap()), snap(&q));
    // decline из WebUI по-прежнему отвечает вопрос
    let dec = decide_idea(&db, i.id, nd(IdeaDecision::Decline), &human())
        .await
        .unwrap();
    assert_eq!(dec.question.unwrap().status, Some(EntryStatus::Answered));
}

/// I2: вопрос-предложение удалён (идея → new) и восстановлен — он устаревает при восстановлении,
/// ответ на него ничего не создаёт.
#[tokio::test]
async fn restored_proposal_becomes_obsolete() {
    let (db, sol, _) = setup("restore-proposal").await;
    let i = new_idea(&db, sol, "Идея", "human", "normal").await;
    let (_, q) = propose_idea(&db, i.id, None, None, None, &agent())
        .await
        .unwrap();
    delete_entry(&db, q.id, None, &human()).await.unwrap();
    let back = restore_entry(&db, q.id, None, &human()).await.unwrap();
    assert_eq!(back.status, Some(EntryStatus::Obsolete));
    let idea = get_entry(&db, i.id).await.unwrap();
    assert_eq!(idea.status, Some(EntryStatus::New));
    let r = answer_question(
        &db,
        q.id,
        answer(Some("accept"), None, AnswerBy::Human),
        &agent(),
    )
    .await;
    assert!(matches!(r, Err(QuestionError::Obsolete)), "{r:?}");
    assert_eq!(
        count(&db, "SELECT count(*) FROM entries WHERE kind = 'task'").await,
        0
    );
    assert_eq!(count(&db, DECISIONS).await, 0);
}

/// I2: ответ на старый вопрос-предложение q1 (идея уже предложена вопросом q2) — 409
/// «предложение устарело» без записи; q2 и идея прежние.
#[tokio::test]
async fn answer_on_stale_proposal_conflict() {
    let (db, sol, _) = setup("stale-proposal").await;
    let i = new_idea(&db, sol, "Идея", "human", "normal").await;
    let (_, q1) = propose_idea(&db, i.id, None, None, None, &agent())
        .await
        .unwrap();
    update_entry(
        &db,
        q1.id,
        None,
        EntryPatch {
            status: Some(EntryStatus::Obsolete),
            reason: Some("неактуально".into()),
            ..EntryPatch::default()
        },
        &agent(),
    )
    .await
    .unwrap();
    let (idea, q2) = propose_idea(&db, i.id, None, None, None, &agent())
        .await
        .unwrap();
    // q1 снова открыт в обход правил — прямым SQL
    exec(
        &db,
        format!("UPDATE entries SET status = 'open' WHERE id = '{}'", q1.id),
    )
    .await
    .unwrap();
    let q1 = get_entry(&db, q1.id).await.unwrap();
    let r = answer_question(
        &db,
        q1.id,
        answer(Some("accept"), None, AnswerBy::Human),
        &agent(),
    )
    .await;
    assert!(
        matches!(r, Err(QuestionError::Idea(IdeaError::StaleProposal))),
        "{r:?}"
    );
    assert_eq!(snap(&get_entry(&db, q1.id).await.unwrap()), snap(&q1));
    assert_eq!(snap(&get_entry(&db, q2.id).await.unwrap()), snap(&q2));
    assert_eq!(snap(&get_entry(&db, i.id).await.unwrap()), snap(&idea));
    assert_eq!(count(&db, DECISIONS).await, 0);
    // ответ на действующий q2 — задача в его task_space
    let ok = answer_question(
        &db,
        q2.id,
        answer(Some("accept"), None, AnswerBy::Human),
        &agent(),
    )
    .await
    .unwrap();
    assert_eq!(ok.task.unwrap().space_id, sol);
}

/// Minor 3: восстановленная идея `proposed`, чей вопрос устарел при удалении, возвращается в
/// `new` без `question_id`.
#[tokio::test]
async fn restored_proposed_idea_returns_to_new() {
    let (db, sol, _) = setup("restore-idea").await;
    let i = new_idea(&db, sol, "Идея", "human", "normal").await;
    let (_, q) = propose_idea(&db, i.id, None, None, None, &agent())
        .await
        .unwrap();
    delete_entry(&db, i.id, None, &human()).await.unwrap();
    assert_eq!(
        get_entry(&db, q.id).await.unwrap().status,
        Some(EntryStatus::Obsolete)
    );
    let back = restore_entry(&db, i.id, None, &human()).await.unwrap();
    assert_eq!(back.status, Some(EntryStatus::New));
    assert_eq!(data_of(&back).question_id, None);
    propose_idea(&db, i.id, None, None, None, &agent())
        .await
        .unwrap();
}

/// Minor 4: `horizon` правкой — те же правила, что у `defer`; `accept` и `decline` его снимают.
#[tokio::test]
async fn horizon_rules_patch_and_decisions() {
    let (db, sol, _) = setup("horizon-rules").await;
    let i = new_idea(&db, sol, "Идея", "human", "normal").await;
    for bad in [today() - days(1), today()] {
        let r = update_entry(
            &db,
            i.id,
            None,
            EntryPatch {
                data: Some(json!({ "horizon": bad.to_string() })),
                ..EntryPatch::default()
            },
            &human(),
        )
        .await;
        assert!(
            matches!(&r, Err(StoreError::BadInput(m)) if m.contains("не раньше завтрашней")),
            "{bad}: {r:?}"
        );
        let mut d = nd(IdeaDecision::Defer);
        d.horizon = Some(bad);
        match decide_idea(&db, i.id, d, &human()).await {
            Err(IdeaError::BadHorizon(m)) => assert!(m.contains("не раньше завтрашней"), "{m}"),
            r => panic!("{r:?}"),
        }
    }
    let e = create_entry(
        &db,
        idea_entry(
            sol,
            "Сразу с датой",
            json!({"origin": "human", "horizon": "2020-01-01"}),
        ),
        &human(),
    )
    .await;
    assert!(matches!(e, Err(StoreError::BadInput(_))), "{e:?}");
    for dec in [IdeaDecision::Accept, IdeaDecision::Decline] {
        let j = new_idea(&db, sol, dec.as_str(), "human", "normal").await;
        decide_idea(&db, j.id, nd(IdeaDecision::Defer), &human())
            .await
            .unwrap();
        let r = decide_idea(&db, j.id, nd(dec), &human()).await.unwrap();
        assert_eq!(data_of(&r.idea).horizon, None, "{dec:?}");
    }
}

/// Minor 5: битый элемент `links` не роняет остальные и не теряется их следующей ревизией.
#[test]
fn stored_links_survive_broken_item() {
    let d = IdeaData::from_stored(&json!({
        "origin": "human",
        "links": ["a", 5, " b ", null, "a", "x".repeat(1001)]
    }));
    assert_eq!(d.links, vec!["a", "b"]);
    assert_eq!(IdeaData::from_stored(&d.to_value()).links, vec!["a", "b"]);
}

/// Восстановление пачки: удалены и идея, и её вопрос-предложение; в любом порядке
/// восстановления идея — `new` без `question_id`, вопрос — `obsolete`.
#[tokio::test]
async fn restore_idea_and_question_any_order() {
    let (db, sol, _) = setup("restore-both").await;
    for idea_first in [true, false] {
        let i = new_idea(&db, sol, &format!("Идея {idea_first}"), "human", "normal").await;
        let (_, q) = propose_idea(&db, i.id, None, None, None, &agent())
            .await
            .unwrap();
        delete_entry(&db, i.id, None, &human()).await.unwrap();
        delete_entry(&db, q.id, None, &human()).await.unwrap();
        let order = if idea_first {
            [i.id, q.id]
        } else {
            [q.id, i.id]
        };
        for id in order {
            restore_entry(&db, id, None, &human()).await.unwrap();
        }
        let idea = get_entry(&db, i.id).await.unwrap();
        let q = get_entry(&db, q.id).await.unwrap();
        assert_eq!(
            idea.status,
            Some(EntryStatus::New),
            "idea_first={idea_first}"
        );
        assert_eq!(data_of(&idea).question_id, None, "idea_first={idea_first}");
        assert_eq!(
            q.status,
            Some(EntryStatus::Obsolete),
            "idea_first={idea_first}"
        );
        assert!(idea.deleted_at.is_none() && q.deleted_at.is_none());
    }
}
