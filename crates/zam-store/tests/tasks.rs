//! Журнал задач для дайджеста (M6.6 §4.2, замечание 28а): [`open_tasks`] — число живых задач
//! `open` пространств и заголовки без задач с живым захватом, порядок — `priority` (`high` →
//! `normal` → `low` → нет), затем новые первыми.

use std::path::PathBuf;

use jiff::SignedDuration;
use serde_json::{Value, json};
use zam_core::Id;
use zam_store::{
    Attribution, AuthorKind, DataDir, Db, EntryKind, EntryPatch, EntryStatus, IdeaPriority,
    NewEntry, SpaceKind, claim_task, create_entry, create_solution, delete_entry, list_spaces,
    open_tasks, update_entry,
};

fn datadir(name: &str) -> DataDir {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("tasks-{name}"));
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

fn agent() -> Attribution {
    Attribution {
        author_kind: AuthorKind::Agent,
        token_id: Some(Id::new()),
        session_id: Some("s-tasks".into()),
        machine: Some("box".into()),
        message: None,
        user_id: Some(Id::new()),
        area: None,
        solution: None,
    }
}

/// Новая задача `open` с `data`; id.
async fn task(db: &Db, space: Id, title: &str, data: Value) -> Id {
    let e = NewEntry {
        refs: None,
        space_id: space,
        kind: EntryKind::Task,
        title: title.into(),
        body: "Шаги задачи.".into(),
        data,
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

async fn set_status(db: &Db, id: Id, status: EntryStatus) {
    let patch = EntryPatch {
        status: Some(status),
        reason: Some("по ходу работы".into()),
        ..EntryPatch::default()
    };
    update_entry(db, id, None, patch, &agent()).await.unwrap();
}

/// M6.6 §4.2: `high` первыми, внутри приоритета — новые; без приоритета — после `low`; задача с
/// живым захватом — в числе, но не в заголовках (она уже в `### Задачи`), с истёкшим — в
/// заголовках; `in_progress`, `blocked`, `done`, удалённые и задачи чужих пространств не в счёт;
/// предел заголовков — `titles`.
#[tokio::test]
async fn open_tasks_count_and_order() {
    let (db, sol, kn) = setup("open").await;
    assert_eq!(
        open_tasks(&db, &[sol, kn], 3).await.unwrap(),
        (0, Vec::new())
    );
    let pr = |p: &str| json!({"priority": p});
    let none = task(&db, sol, "Без приоритета", json!({})).await;
    let low = task(&db, sol, "Низкая", pr("low")).await;
    let high_old = task(&db, sol, "Срочная старая", pr("high")).await;
    let normal = task(&db, sol, "Обычная", pr("normal")).await;
    let kn_normal = task(&db, kn, "Обычная из Knowledge", pr("normal")).await;
    let claimed = task(&db, sol, "Захваченная", pr("high")).await;
    let expired = task(&db, sol, "Захват истёк", pr("high")).await;
    let high_new = task(&db, sol, "Срочная новая", pr("high")).await;
    // не в счёте: в работе, заблокирована, сделана, удалена
    for (title, status) in [
        ("В работе", Some(EntryStatus::InProgress)),
        ("Заблокирована", Some(EntryStatus::Blocked)),
        ("Сделана", Some(EntryStatus::Done)),
        ("Удалена", None),
    ] {
        let id = task(&db, sol, title, pr("high")).await;
        match status {
            Some(s) => set_status(&db, id, s).await,
            None => delete_entry(&db, id, None, &agent()).await.unwrap(),
        }
    }
    let now = zam_store::time::now();
    claim_task(&db, claimed, "s-live", None, 60, now)
        .await
        .unwrap();
    // захват на минуту два часа назад — истёк
    claim_task(
        &db,
        expired,
        "s-old",
        None,
        1,
        now - SignedDuration::from_hours(2),
    )
    .await
    .unwrap();

    let (n, titles) = open_tasks(&db, &[sol, kn], 10).await.unwrap();
    assert_eq!(
        n, 8,
        "все живые open решения и Knowledge, захваченная — тоже"
    );
    let got: Vec<(Id, &str, Option<IdeaPriority>)> = titles
        .iter()
        .map(|t| (t.id, t.title.as_str(), t.priority))
        .collect();
    assert_eq!(
        got,
        [
            (high_new, "Срочная новая", Some(IdeaPriority::High)),
            (expired, "Захват истёк", Some(IdeaPriority::High)),
            (high_old, "Срочная старая", Some(IdeaPriority::High)),
            (
                kn_normal,
                "Обычная из Knowledge",
                Some(IdeaPriority::Normal)
            ),
            (normal, "Обычная", Some(IdeaPriority::Normal)),
            (low, "Низкая", Some(IdeaPriority::Low)),
            (none, "Без приоритета", None),
        ]
    );
    assert!(titles.iter().all(|t| t.id != claimed), "{titles:?}");
    // время создания — для слияния заголовков нескольких баз: новые первыми внутри приоритета
    assert!(titles[0].created_at > titles[1].created_at, "{titles:?}");

    // предел заголовков и одно пространство: число — всех открытых пространства
    let (n, titles) = open_tasks(&db, &[sol], 2).await.unwrap();
    assert_eq!(n, 7);
    let ids: Vec<Id> = titles.iter().map(|t| t.id).collect();
    assert_eq!(ids, [high_new, expired]);
    let (n, titles) = open_tasks(&db, &[kn], 3).await.unwrap();
    assert_eq!((n, titles.len()), (1, 1));
    // пустой список пространств — ноль без запроса
    assert_eq!(open_tasks(&db, &[], 3).await.unwrap(), (0, Vec::new()));
}
