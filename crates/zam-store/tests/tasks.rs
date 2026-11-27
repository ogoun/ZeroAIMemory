//! Журнал задач для дайджеста (M6.6 §4.2, замечание 28а): [`open_tasks`] — число живых задач
//! `open` пространств и заголовки без задач с живым захватом, порядок — `priority` (`high` →
//! `normal` → `low` → нет), затем новые первыми. Дерево подзадач `part_of` ([`task_tree`]) и
//! фильтры ленты по спискам id (M7 §5.4, задача 5 M7.2).

use std::path::PathBuf;

use jiff::SignedDuration;
use serde_json::{Value, json};
use zam_core::Id;
use zam_store::{
    Attribution, AuthorKind, DataDir, Db, EdgeType, EntryFilter, EntryKind, EntryPatch,
    EntryStatus, GraphStore, IdeaPriority, NewEdge, NewEntry, OpenTask, SpaceKind, StoreError,
    claim_task, count_by_status, create_entry, create_solution, delete_entry, list_entries,
    list_spaces, open_tasks, space_counts, task_tree, update_entry,
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
    // ревью задачи 3, M3: порядок SQL — тот же, что у ключа слияния `order_key` (один источник
    // ранга приоритета — `IdeaPriority::rank`)
    let mut by_key = titles.clone();
    by_key.reverse();
    by_key.sort_by_key(OpenTask::order_key);
    assert_eq!(by_key, titles);

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

// ---------- Дерево подзадач `part_of` (M7 §5.4, задача 5 M7.2) ----------

/// Связь `part_of` `step → parent`; id ребра.
async fn part_of(db: &Db, step: Id, parent: Id) -> Id {
    let e = |id| zam_core::TargetRef {
        kind: zam_core::TargetKind::Entry,
        id,
    };
    db.add_edge(
        NewEdge {
            src: e(step),
            dst: e(parent),
            kind: EdgeType::PartOf,
        },
        &agent(),
    )
    .await
    .unwrap()
    .id
}

/// [`task_tree`]: одним запросом — задачи пространства и их рёбра `part_of` в порядке связей;
/// рёбра в другое пространство, удалённые рёбра и удалённые задачи не в счёт; узел несёт
/// заголовок, статус, `updated_at` и захват.
#[tokio::test]
async fn task_tree_space_order_and_deleted() {
    let (db, sol, kn) = setup("tree").await;
    let p = task(&db, sol, "Общая", json!({})).await;
    let s: Vec<Id> = futures_steps(&db, sol, 4).await;
    let t_kn = task(&db, kn, "Шаг Knowledge", json!({})).await;
    part_of(&db, s[2], p).await;
    part_of(&db, s[0], p).await;
    part_of(&db, t_kn, p).await;
    let gone = part_of(&db, s[1], p).await;
    part_of(&db, s[3], p).await;
    db.remove_edge(gone, &agent()).await.unwrap();
    delete_entry(&db, s[3], None, &agent()).await.unwrap();
    set_status(&db, s[0], EntryStatus::Done).await;
    claim_task(&db, s[2], "cs", Some("claude"), 30, zam_store::time::now())
        .await
        .unwrap();
    let tree = task_tree(&db, sol).await.unwrap();
    assert_eq!(tree.children_of(p), [s[2], s[0]]);
    assert_eq!(tree.parent.get(&s[2]), Some(&p));
    assert!(!tree.parent.contains_key(&s[1]) && !tree.nodes.contains_key(&s[3]));
    assert!(
        !tree.nodes.contains_key(&t_kn),
        "задача другого пространства"
    );
    let n = &tree.nodes[&s[2]];
    assert_eq!((n.title.as_str(), n.status), ("Шаг 2", EntryStatus::Open));
    assert_eq!(n.claim_agent.as_deref(), Some("claude"));
    assert!(n.claimed(zam_store::time::now()));
    let sum = tree.summary(p).unwrap();
    assert_eq!((sum.total, sum.done, sum.open), (2, 1, 1));
    // в Knowledge шаг — верхнего уровня: ребро ведёт в другое пространство
    let kt = task_tree(&db, kn).await.unwrap();
    assert!(kt.parent.is_empty() && kt.nodes.contains_key(&t_kn));
}

/// `task_steps` [`space_counts`] (M7.3 §6.14 п. 10) — число подзадач дерева [`task_tree`] того же
/// пространства: два родителя — один шаг, вложенный шаг — шаг, задачи цикла `part_of` — верхнего
/// уровня (а задача, ведущая в цикл, — шаг), удалённое ребро, удалённая задача и рёбра в другое
/// пространство (в обе стороны) не в счёт; у Knowledge с ребром наружу — 0.
#[tokio::test]
async fn space_counts_task_steps_match_tree() {
    let (db, sol, kn) = setup("task-steps").await;
    let p = task(&db, sol, "Общая", json!({})).await;
    let q = task(&db, sol, "Вторая общая", json!({})).await;
    let s = futures_steps(&db, sol, 4).await;
    for x in &s {
        part_of(&db, *x, p).await;
    }
    part_of(&db, s[1], q).await;
    let nested = task(&db, sol, "Шаг шага", json!({})).await;
    part_of(&db, nested, s[0]).await;
    let mut c: Vec<Id> = Vec::new();
    for t in ["Цикл А", "Цикл Б", "Ведёт в цикл"] {
        c.push(task(&db, sol, t, json!({})).await);
    }
    part_of(&db, c[0], c[1]).await;
    part_of(&db, c[1], c[0]).await;
    part_of(&db, c[2], c[0]).await;
    let loose = task(&db, sol, "Отвязанная", json!({})).await;
    let gone = part_of(&db, loose, p).await;
    db.remove_edge(gone, &agent()).await.unwrap();
    delete_entry(&db, s[3], None, &agent()).await.unwrap();
    let k = task(&db, kn, "Задача Knowledge", json!({})).await;
    part_of(&db, k, p).await;
    let out = task(&db, sol, "Шаг задачи Knowledge", json!({})).await;
    part_of(&db, out, k).await;

    let spaces = list_spaces(&db).await.unwrap();
    let space = |id: Id| spaces.iter().find(|x| x.id == id).unwrap();
    let tree = task_tree(&db, sol).await.unwrap();
    let c_sol = space_counts(&db, space(sol), true).await.unwrap();
    // s0, s1 (два родителя), s2, шаг шага, c2 (ведёт в цикл)
    assert_eq!(c_sol.task_steps, 5);
    assert_eq!(
        usize::try_from(c_sol.task_steps).unwrap(),
        tree.parent.len()
    );
    // задачи доски верхнего уровня — число журнала без шагов
    let top = count_by_status(
        &db,
        EntryFilter {
            space_id: sol,
            kind: Some(EntryKind::Task),
            except_ids: tree.subtask_ids(),
            limit: 1,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let top: u64 = top.iter().map(|(_, v)| u64::try_from(*v).unwrap()).sum();
    assert_eq!(top + c_sol.task_steps, c_sol.journals["tasks"]);
    let c_kn = space_counts(&db, space(kn), true).await.unwrap();
    assert_eq!(c_kn.task_steps, 0);
    assert_eq!(task_tree(&db, kn).await.unwrap().parent.len(), 0);
}

/// `n` задач «Шаг i» пространства `space` по порядку создания.
async fn futures_steps(db: &Db, space: Id, n: usize) -> Vec<Id> {
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        out.push(task(db, space, &format!("Шаг {i}"), json!({})).await);
    }
    out
}

/// Фильтры ленты по спискам id (M7 §5.4 п. 3): `only_ids` с `ids_order` — порядок списка и
/// курсор-позиция (повторы id не в счёт, пустой список — пустая лента), `except_ids` — без них;
/// счётчики статусов — с теми же списками; `ids_order` без `only_ids` — `BadInput`.
#[tokio::test]
async fn entry_filter_id_lists() {
    let (db, sol, _) = setup("id-lists").await;
    let s = futures_steps(&db, sol, 5).await;
    set_status(&db, s[1], EntryStatus::Done).await;
    let base = EntryFilter {
        space_id: sol,
        kind: Some(EntryKind::Task),
        limit: 2,
        ..Default::default()
    };
    let order = vec![s[3], s[0], s[3], s[4], s[1]];
    let mut got = Vec::new();
    let mut cursor = None;
    loop {
        let f = EntryFilter {
            only_ids: Some(order.clone()),
            ids_order: true,
            cursor: cursor.clone(),
            ..base.clone()
        };
        let (items, next) = list_entries(&db, f).await.unwrap();
        got.extend(items.iter().map(|e| e.id));
        match next {
            Some(c) => cursor = Some(c),
            None => break,
        }
    }
    assert_eq!(got, [s[3], s[0], s[4], s[1]]);
    let only = |ids: Vec<Id>| EntryFilter {
        only_ids: Some(ids),
        limit: 200,
        ..base.clone()
    };
    assert!(
        list_entries(&db, only(Vec::new()))
            .await
            .unwrap()
            .0
            .is_empty()
    );
    // без порядка списка — новые первыми
    let (items, _) = list_entries(&db, only(vec![s[0], s[4]])).await.unwrap();
    assert_eq!(items.iter().map(|e| e.id).collect::<Vec<_>>(), [s[4], s[0]]);
    let f = EntryFilter {
        except_ids: vec![s[0], s[1], s[2]],
        limit: 200,
        ..base.clone()
    };
    let (items, _) = list_entries(&db, f.clone()).await.unwrap();
    assert_eq!(items.iter().map(|e| e.id).collect::<Vec<_>>(), [s[4], s[3]]);
    let counts = count_by_status(&db, f).await.unwrap();
    assert!(counts.contains(&(EntryStatus::Open, 2)) && counts.contains(&(EntryStatus::Done, 0)));
    let counts = count_by_status(&db, only(vec![s[1], s[2]])).await.unwrap();
    assert!(counts.contains(&(EntryStatus::Open, 1)) && counts.contains(&(EntryStatus::Done, 1)));
    let bad = EntryFilter {
        ids_order: true,
        ..base.clone()
    };
    assert!(matches!(
        list_entries(&db, bad).await,
        Err(StoreError::BadInput(_))
    ));
    let bad_cursor = EntryFilter {
        only_ids: Some(order),
        ids_order: true,
        cursor: Some("2026-11-22T10:00:00Z|x".into()),
        ..base
    };
    assert!(matches!(
        list_entries(&db, bad_cursor).await,
        Err(StoreError::BadInput(_))
    ));
}

/// Цена [`task_tree`] на 2 000 задач пространства (1 500 из них — шаги 100 общих задач, план
/// `docs/plans/2026-11-22-m7.2-webui.md`, задача 5, шаг 4): медиана и наибольшее из 20 построений —
/// в журнал этапа. Запуск: `cargo test -p zam-store --test tasks task_tree_2000_cost --release --
/// --ignored --nocapture`.
#[tokio::test]
#[ignore = "замер для журнала этапа"]
async fn task_tree_2000_cost() {
    let (db, sol, _) = setup("tree-2000").await;
    let umbrellas = futures_steps(&db, sol, 100).await;
    let steps = futures_steps(&db, sol, 1_900).await;
    for (i, s) in steps.iter().take(1_500).enumerate() {
        part_of(&db, *s, umbrellas[i % 100]).await;
    }
    let mut times = Vec::new();
    for _ in 0..20 {
        let t = std::time::Instant::now();
        let tree = task_tree(&db, sol).await.unwrap();
        times.push(t.elapsed());
        assert_eq!((tree.nodes.len(), tree.parent.len()), (2_000, 1_500));
    }
    times.sort();
    println!(
        "task_tree 2000 задач, 1500 рёбер: медиана {:?}, наибольшее {:?}",
        times[times.len() / 2],
        times[times.len() - 1]
    );
}
