//! Граф связей (§8.5 ТЗ, задача 10 M3): рёбра из вики-ссылок, Markdown-ссылок и `relations:`
//! frontmatter, явные и системные рёбра, разрешение ссылок при изменении дерева, соседи с
//! фильтром пространств, дозаполнение рёбер при открытии базы.

use std::collections::BTreeSet;
use std::path::PathBuf;

use jiff::Timestamp;
use serde_json::json;
use zam_core::{Id, TargetKind, TargetRef};
use zam_markdown::{Link, LinkKind};
use zam_store::{
    Attribution, AuthorKind, Certainty, DataDir, Db, EdgeOrigin, EdgeType, EdgeView, EntryKind,
    GraphStore, NewEdge, NewEntry, NewNote, NodePath, SpaceKind, StoreError, USER_MIGRATIONS,
    UserDbs, create_entry, create_note, create_solution, delete_node, find_by_name, links,
    list_spaces, move_node, restore_node, supersede_entry, update_note,
};

fn datadir(name: &str) -> DataDir {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("graph-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    DataDir::open(&dir).unwrap()
}

/// База пользователя и id пространства Knowledge.
async fn setup(name: &str) -> (Db, Id) {
    let dd = datadir(name);
    let db = UserDbs::new(dd.users_dir()).get(Id::new()).await.unwrap();
    let k = knowledge(&db).await;
    (db, k)
}

async fn knowledge(db: &Db) -> Id {
    list_spaces(db)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.kind == SpaceKind::Knowledge)
        .unwrap()
        .id
}

fn attrib(kind: AuthorKind) -> Attribution {
    Attribution {
        author_kind: kind,
        token_id: Some(Id::new()),
        session_id: None,
        machine: None,
        message: Some("правка".into()),
        user_id: Some(Id::new()),
        area: None,
        solution: None,
    }
}

fn agent() -> Attribution {
    attrib(AuthorKind::Agent)
}

fn human() -> Attribution {
    attrib(AuthorKind::Human)
}

/// Создаёт заметку от агента; возвращает её id.
async fn note(db: &Db, space: Id, path: &str, content: &str) -> Id {
    note_by(db, space, path, content, agent()).await
}

async fn note_by(db: &Db, space: Id, path: &str, content: &str, a: Attribution) -> Id {
    create_note(
        db,
        NewNote {
            space_id: space,
            path: NodePath::parse(path).unwrap(),
            content: content.into(),
            l0: None,
            attrib: a,
        },
    )
    .await
    .unwrap()
    .id
}

fn n(id: Id) -> TargetRef {
    TargetRef {
        kind: TargetKind::Node,
        id,
    }
}

fn e(id: Id) -> TargetRef {
    TargetRef {
        kind: TargetKind::Entry,
        id,
    }
}

/// Все пространства базы (область видимости владельца).
async fn all_spaces(db: &Db) -> Vec<Id> {
    list_spaces(db)
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect()
}

/// Исходящие рёбра `src` как `(тип, происхождение, цель, dst_raw)`, по порядку ответа.
async fn out(
    db: &Db,
    src: TargetRef,
) -> Vec<(EdgeType, EdgeOrigin, Option<TargetRef>, Option<String>)> {
    let spaces = all_spaces(db).await;
    db.outgoing(src, &spaces)
        .await
        .unwrap()
        .into_iter()
        .map(|e| (e.kind, e.origin, e.dst, e.dst_raw))
        .collect()
}

/// Цель единственного исходящего ребра с `dst_raw = raw`.
async fn dst_of(db: &Db, src: TargetRef, raw: &str) -> Option<TargetRef> {
    let edges = out(db, src).await;
    let found: Vec<_> = edges
        .iter()
        .filter(|e| e.3.as_deref() == Some(raw))
        .collect();
    assert_eq!(found.len(), 1, "ребро {raw}: {edges:?}");
    found[0].2
}

async fn backlink_srcs(db: &Db, target: TargetRef, spaces: &[Id]) -> Vec<TargetRef> {
    db.backlinks(target, spaces)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.src)
        .collect()
}

async fn rev(db: &Db, id: Id) -> i64 {
    zam_store::get_node(db, id).await.unwrap().current_rev
}

fn insight(space: Id, title: &str, certainty: Option<Certainty>) -> NewEntry {
    NewEntry {
        refs: None,
        space_id: space,
        kind: EntryKind::Insight,
        title: title.into(),
        body: "тело".into(),
        data: json!({"kind": "gotcha"}),
        area: None,
        tags: Vec::new(),
        status: None,
        certainty,
        idempotency_key: None,
        triggers: Vec::new(),
        links: Vec::new(),
    }
}

#[tokio::test]
async fn wikilinks_create_resolved_and_unresolved() {
    let (db, k) = setup("wikilinks").await;
    let b = note(&db, k, "notes/b", "# B\n\n## Раздел\n\nтекст\n").await;
    let c = note(&db, k, "notes/c", "# C\n").await;
    let a = note(
        &db,
        k,
        "notes/a",
        "# A\n\nСм. [[b#Раздел|текст]], [[Нет такой]], [про c](c.md), \
         [внешняя](https://example.com/x.md), [якорь](#a) и ![картинка](c).\n\
         Повтор [[b]] — то же ребро.\n",
    )
    .await;
    let spaces = all_spaces(&db).await;
    let edges: Vec<EdgeView> = db.outgoing(n(a), &spaces).await.unwrap();
    assert_eq!(edges.len(), 3, "{edges:?}");
    let by_raw = |raw: &str| {
        edges
            .iter()
            .find(|e| e.dst_raw.as_deref() == Some(raw))
            .unwrap()
    };
    let wb = by_raw("[[b]]");
    assert_eq!(wb.dst, Some(n(b)));
    assert_eq!(wb.dst_space, Some(k));
    assert_eq!(wb.dst_anchor.as_deref(), Some("Раздел"));
    assert_eq!(
        (wb.kind, wb.origin),
        (EdgeType::LinksTo, EdgeOrigin::Wikilink)
    );
    assert_eq!((wb.src, wb.src_space), (n(a), k));
    let none = by_raw("[[Нет такой]]");
    assert_eq!((none.dst, none.dst_space), (None, None));
    // Markdown-ссылка на относительный путь — тоже links_to (путь от папки заметки, без .md)
    let mc = by_raw("notes/c.md");
    assert_eq!(mc.dst, Some(n(c)));
    assert_eq!(mc.kind, EdgeType::LinksTo);
    assert_eq!(backlink_srcs(&db, n(b), &spaces).await, vec![n(a)]);
    assert_eq!(backlink_srcs(&db, n(c), &spaces).await, vec![n(a)]);

    // relations: во frontmatter — свой тип, origin = frontmatter; неизвестный тип пропускается
    let d = note(
        &db,
        k,
        "notes/d",
        "---\nrelations: [\"depends_on: [[b]]\", \"fixes: notes/c\", \"нечто: [[b]]\"]\n---\n# D\n",
    )
    .await;
    let mut got = out(&db, n(d)).await;
    got.sort_by_key(|e| e.0.as_str());
    assert_eq!(
        got,
        vec![
            (
                EdgeType::DependsOn,
                EdgeOrigin::Frontmatter,
                Some(n(b)),
                Some("[[b]]".into())
            ),
            (
                EdgeType::Fixes,
                EdgeOrigin::Frontmatter,
                Some(n(c)),
                Some("notes/c".into())
            ),
        ]
    );
    let doc = zam_markdown::parse(
        "---\nrelations: [\"нечто: [[b]]\", \"depends_on: [[b]]\", \"без двоеточия\"]\n---\n",
    );
    assert_eq!(
        links::relation_warnings(&doc),
        vec![
            "unknown relation type: нечто".to_string(),
            "invalid relation: без двоеточия".to_string()
        ]
    );
}

#[tokio::test]
async fn unresolved_resolves_when_target_created() {
    let (db, k) = setup("pending").await;
    let a = note(&db, k, "a", "[[Позже]] и [[x/Путь]]\n").await;
    assert_eq!(dst_of(&db, n(a), "[[Позже]]").await, None);
    let later = note(&db, k, "x/Позже", "текст\n").await;
    assert_eq!(dst_of(&db, n(a), "[[Позже]]").await, Some(n(later)));
    assert_eq!(dst_of(&db, n(a), "[[x/Путь]]").await, None);
    let path = note(&db, k, "x/Путь", "текст\n").await;
    assert_eq!(dst_of(&db, n(a), "[[x/Путь]]").await, Some(n(path)));
    // заметка в другом пространстве ссылку не разрешает: вики-ссылки — в пространстве источника
    let (s, _) = create_solution(&db, &zam_core::SolutionGuid::new_v7().to_string(), "S")
        .await
        .unwrap();
    let a2 = note(&db, s.id, "a2", "[[Позже]]\n").await;
    assert_eq!(dst_of(&db, n(a2), "[[Позже]]").await, None);
}

#[tokio::test]
async fn rename_and_move_reresolve() {
    let (db, k) = setup("rename").await;
    let b = note(&db, k, "k/B", "b\n").await;
    let d = note(&db, k, "d", "[[C]]\n").await;
    let a = note(&db, k, "a", "[[B]] и [[k/B]]\n").await;
    assert_eq!(dst_of(&db, n(a), "[[B]]").await, Some(n(b)));
    assert_eq!(dst_of(&db, n(a), "[[k/B]]").await, Some(n(b)));
    // перенос с тем же именем: по имени — всё ещё B, по старому пути — уже нет
    move_node(&db, b, NodePath::parse("m/B").unwrap(), AuthorKind::Agent)
        .await
        .unwrap();
    assert_eq!(dst_of(&db, n(a), "[[B]]").await, Some(n(b)));
    assert_eq!(dst_of(&db, n(a), "[[k/B]]").await, None);
    // переименование: старое имя не разрешается, ожидавшая [[C]] — разрешается
    move_node(&db, b, NodePath::parse("m/C").unwrap(), AuthorKind::Agent)
        .await
        .unwrap();
    assert_eq!(dst_of(&db, n(a), "[[B]]").await, None);
    assert_eq!(dst_of(&db, n(d), "[[C]]").await, Some(n(b)));
    // перенос папки: относительная Markdown-ссылка заметки внутри считается от нового места
    let g = note(&db, k, "f/G", "g\n").await;
    let f = note(&db, k, "f/F", "[к G](G)\n").await;
    assert_eq!(dst_of(&db, n(f), "f/G").await, Some(n(g)));
    let folder = zam_store::get_node_by_path(&db, k, &NodePath::parse("f").unwrap())
        .await
        .unwrap()
        .id;
    move_node(
        &db,
        folder,
        NodePath::parse("z").unwrap(),
        AuthorKind::Agent,
    )
    .await
    .unwrap();
    // G переехал вместе с F: ссылка указывает на z/G — тот же узел
    assert_eq!(dst_of(&db, n(f), "z/G").await, Some(n(g)));
}

#[tokio::test]
async fn same_name_resolves_first_by_path_like_render() {
    let (db, k) = setup("same-name").await;
    let late = note(&db, k, "b/N", "поздний путь\n").await;
    let a = note(&db, k, "src", "[[N]]\n").await;
    assert_eq!(dst_of(&db, n(a), "[[N]]").await, Some(n(late)));
    // новая заметка с тем же именем и меньшим путём перехватывает ссылку — как в рендере
    let early = note(&db, k, "a/N", "ранний путь\n").await;
    assert_eq!(dst_of(&db, n(a), "[[N]]").await, Some(n(early)));
    let first = find_by_name(&db, k, "N").await.unwrap()[0].id;
    assert_eq!(first, early);
    let link = Link {
        kind: LinkKind::Wiki,
        target: "N".into(),
        anchor: None,
        label: None,
        range: 0..0,
    };
    let shared = db
        .read(move |c| Ok(links::resolve_link(c, k, "src", &link)?))
        .await
        .unwrap();
    assert_eq!(shared, Some(early));
    // удаление — снова первая по пути из живых; восстановление — обратно
    delete_node(&db, early, AuthorKind::Agent).await.unwrap();
    assert_eq!(dst_of(&db, n(a), "[[N]]").await, Some(n(late)));
    restore_node(&db, early, AuthorKind::Agent).await.unwrap();
    assert_eq!(dst_of(&db, n(a), "[[N]]").await, Some(n(early)));
}

#[tokio::test]
async fn rewrite_replaces_derived_keeps_explicit() {
    let (db, k) = setup("rewrite").await;
    let b = note(&db, k, "b", "b\n").await;
    let c = note(&db, k, "c", "c\n").await;
    let a = note(&db, k, "a", "[[b]]\n").await;
    let ex = db
        .add_edge(
            NewEdge {
                src: n(a),
                dst: n(c),
                kind: EdgeType::RelatesTo,
            },
            &agent(),
        )
        .await
        .unwrap();
    assert_eq!(
        (ex.origin, ex.dst, ex.dst_raw.clone()),
        (EdgeOrigin::Explicit, Some(n(c)), None)
    );
    // то же явное ребро повторно — 409
    let dup = db
        .add_edge(
            NewEdge {
                src: n(a),
                dst: n(c),
                kind: EdgeType::RelatesTo,
            },
            &agent(),
        )
        .await;
    assert!(matches!(dup, Err(StoreError::Conflict(_))), "{dup:?}");
    update_note(&db, a, rev(&db, a).await, "[[c]]\n".into(), None, agent())
        .await
        .unwrap();
    let mut got = out(&db, n(a)).await;
    got.sort_by_key(|e| e.0.as_str());
    assert_eq!(
        got,
        vec![
            (
                EdgeType::LinksTo,
                EdgeOrigin::Wikilink,
                Some(n(c)),
                Some("[[c]]".into())
            ),
            (EdgeType::RelatesTo, EdgeOrigin::Explicit, Some(n(c)), None),
        ]
    );
    let spaces = all_spaces(&db).await;
    assert!(backlink_srcs(&db, n(b), &spaces).await.is_empty());
    // выведенное ребро не удаляется вручную; явное — мягко
    let derived = db
        .outgoing(n(a), &spaces)
        .await
        .unwrap()
        .into_iter()
        .find(|e| e.origin == EdgeOrigin::Wikilink)
        .unwrap();
    assert!(matches!(
        db.remove_edge(derived.id, &agent()).await,
        Err(StoreError::BadInput(_))
    ));
    db.remove_edge(ex.id, &agent()).await.unwrap();
    assert!(matches!(
        db.remove_edge(ex.id, &agent()).await,
        Err(StoreError::NotFound)
    ));
    assert_eq!(out(&db, n(a)).await.len(), 1);
    let soft: i64 = db
        .read(move |c| {
            Ok(c.query_row(
                "SELECT count(*) FROM edges WHERE id = ?1 AND deleted_at IS NOT NULL",
                [ex.id],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(soft, 1);
    // после удаления явное ребро можно создать снова
    db.add_edge(
        NewEdge {
            src: n(a),
            dst: n(c),
            kind: EdgeType::RelatesTo,
        },
        &agent(),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn explicit_edge_axiom_source_allowed_to_agent() {
    let (db, k) = setup("axiom-edge").await;
    let ax = note_by(
        &db,
        k,
        "ax",
        "---
certainty: axiom
---
истина
",
        human(),
    )
    .await;
    let plain = note(
        &db,
        k,
        "plain",
        "текст
",
    )
    .await;
    let to = |src, dst| NewEdge {
        src: n(src),
        dst: n(dst),
        kind: EdgeType::DependsOn,
    };
    // с M4 ребро от аксиомы и к ней агент создаёт и удаляет сам (§8.5, решение владельца
    // 2026-10-26: ruling R10 снят)
    let from_ax = db.add_edge(to(ax, plain), &agent()).await.unwrap();
    db.add_edge(to(plain, ax), &agent()).await.unwrap();
    db.remove_edge(from_ax.id, &agent()).await.unwrap();
    let by_human = db.add_edge(to(ax, plain), &human()).await.unwrap();
    db.remove_edge(by_human.id, &agent()).await.unwrap();
    // supersedes явным не бывает; несуществующая цель — NotFound; петля — BadInput
    let r = db
        .add_edge(
            NewEdge {
                src: n(plain),
                dst: n(ax),
                kind: EdgeType::Supersedes,
            },
            &human(),
        )
        .await;
    assert!(matches!(r, Err(StoreError::BadInput(_))), "{r:?}");
    let r = db.add_edge(to(plain, Id::new()), &agent()).await;
    assert!(matches!(r, Err(StoreError::NotFound)), "{r:?}");
    let r = db.add_edge(to(plain, plain), &agent()).await;
    assert!(matches!(r, Err(StoreError::BadInput(_))), "{r:?}");
}

#[tokio::test]
async fn delete_source_hides_backlinks_restore_returns() {
    let (db, k) = setup("delete-source").await;
    let b = note(&db, k, "b", "b\n").await;
    let a = note(&db, k, "a", "[[b]]\n").await;
    let spaces = all_spaces(&db).await;
    assert_eq!(backlink_srcs(&db, n(b), &spaces).await, vec![n(a)]);
    delete_node(&db, a, AuthorKind::Agent).await.unwrap();
    assert!(backlink_srcs(&db, n(b), &spaces).await.is_empty());
    let g = db.neighbors(&[n(b)], 1, &[], &spaces).await.unwrap();
    assert_eq!(g.nodes.len(), 1, "{g:?}");
    assert!(g.edges.is_empty());
    restore_node(&db, a, AuthorKind::Agent).await.unwrap();
    assert_eq!(backlink_srcs(&db, n(b), &spaces).await, vec![n(a)]);
    // удалённая цель: ссылка становится неразрешённой, восстановление разрешает её снова
    delete_node(&db, b, AuthorKind::Agent).await.unwrap();
    assert_eq!(dst_of(&db, n(a), "[[b]]").await, None);
    restore_node(&db, b, AuthorKind::Agent).await.unwrap();
    assert_eq!(dst_of(&db, n(a), "[[b]]").await, Some(n(b)));
}

#[tokio::test]
async fn neighbors_depth2_cte_cycle_safe() {
    let (db, k) = setup("neighbors").await;
    // цепочка a → b → c → d и цикл c → a; явное a → x (relates_to)
    let d = note(&db, k, "d", "d\n").await;
    let c = note(&db, k, "c", "[[d]] [[a]]\n").await;
    let b = note(&db, k, "b", "[[c]]\n").await;
    let a = note(&db, k, "a", "[[b]]\n").await;
    // c ссылалась на a до его создания — теперь разрешено
    assert_eq!(dst_of(&db, n(c), "[[a]]").await, Some(n(a)));
    let x = note(&db, k, "x", "x\n").await;
    db.add_edge(
        NewEdge {
            src: n(a),
            dst: n(x),
            kind: EdgeType::RelatesTo,
        },
        &agent(),
    )
    .await
    .unwrap();
    let spaces = all_spaces(&db).await;
    let ids = |g: &zam_store::Graph| g.nodes.iter().map(|n| n.r).collect::<BTreeSet<_>>();
    let g1 = db.neighbors(&[n(a)], 1, &[], &spaces).await.unwrap();
    // в обе стороны: b (исходящее), c (входящее c → a), x (явное)
    assert_eq!(ids(&g1), BTreeSet::from([n(a), n(b), n(c), n(x)]));
    let g2 = db.neighbors(&[n(a)], 2, &[], &spaces).await.unwrap();
    assert_eq!(ids(&g2), BTreeSet::from([n(a), n(b), n(c), n(d), n(x)]));
    // цикл не размножает узлы и рёбра
    let g3 = db.neighbors(&[n(a)], 3, &[], &spaces).await.unwrap();
    assert_eq!(g3.nodes.len(), 5);
    let edge_ids: BTreeSet<Id> = g3.edges.iter().map(|e| e.id).collect();
    assert_eq!(edge_ids.len(), g3.edges.len());
    assert_eq!(g3.edges.len(), 5, "{:?}", g3.edges);
    let node_a = g3.nodes.iter().find(|g| g.r == n(a)).unwrap();
    assert_eq!(
        (node_a.title.as_str(), node_a.path.as_deref()),
        ("a", Some("a"))
    );
    assert!(!node_a.deleted);
    // фильтр типов
    let only_rel = db
        .neighbors(&[n(a)], 2, &[EdgeType::RelatesTo], &spaces)
        .await
        .unwrap();
    assert_eq!(ids(&only_rel), BTreeSet::from([n(a), n(x)]));
    let links = db
        .neighbors(&[n(a)], 1, &[EdgeType::LinksTo], &spaces)
        .await
        .unwrap();
    assert_eq!(ids(&links), BTreeSet::from([n(a), n(b), n(c)]));
    // глубина вне 1..=3 — BadInput
    assert!(matches!(
        db.neighbors(&[n(a)], 4, &[], &spaces).await,
        Err(StoreError::BadInput(_))
    ));
    assert!(matches!(
        db.neighbors(&[n(a)], 0, &[], &spaces).await,
        Err(StoreError::BadInput(_))
    ));
}

#[tokio::test]
async fn neighbors_respect_spaces_filter() {
    let (db, k) = setup("spaces").await;
    let (s, _) = create_solution(&db, &zam_core::SolutionGuid::new_v7().to_string(), "S")
        .await
        .unwrap();
    let kn = note(&db, k, "знание", "секрет владельца\n").await;
    let sol = note(&db, s.id, "решение", "заметка решения\n").await;
    let ins = create_entry(&db, insight(s.id, "вывод решения", None), &agent())
        .await
        .unwrap()
        .0;
    // в пределах одной базы связь между пространствами допустима
    for (src, dst) in [(e(ins.id), n(kn)), (n(sol), e(ins.id)), (n(kn), n(sol))] {
        db.add_edge(
            NewEdge {
                src,
                dst,
                kind: EdgeType::RelatesTo,
            },
            &agent(),
        )
        .await
        .unwrap();
    }
    let both = [k, s.id];
    let only_s = [s.id];
    let g = db.neighbors(&[e(ins.id)], 1, &[], &both).await.unwrap();
    assert_eq!(g.nodes.len(), 3);
    // участнику (только пространство решения) Knowledge владельца не виден ни узлом, ни ребром
    let g = db.neighbors(&[e(ins.id)], 3, &[], &only_s).await.unwrap();
    let refs: BTreeSet<TargetRef> = g.nodes.iter().map(|n| n.r).collect();
    assert_eq!(refs, BTreeSet::from([e(ins.id), n(sol)]));
    assert!(
        g.edges
            .iter()
            .all(|x| x.src_space == s.id && x.dst_space == Some(s.id))
    );
    assert_eq!(g.edges.len(), 1);
    assert!(backlink_srcs(&db, n(sol), &only_s).await.is_empty());
    assert_eq!(backlink_srcs(&db, n(sol), &both).await, vec![n(kn)]);
    let outs = db.outgoing(e(ins.id), &only_s).await.unwrap();
    assert!(outs.is_empty(), "{outs:?}");
    assert_eq!(db.outgoing(e(ins.id), &both).await.unwrap().len(), 1);
    // пустая область — ничего
    assert!(db.backlinks(n(sol), &[]).await.unwrap().is_empty());
}

#[tokio::test]
async fn supersede_creates_system_edge() {
    let (db, k) = setup("supersede").await;
    let old = create_entry(&db, insight(k, "старый вывод", None), &agent())
        .await
        .unwrap()
        .0;
    let (new, _) = supersede_entry(&db, old.id, None, insight(k, "новый вывод", None), &agent())
        .await
        .unwrap();
    let edges = out(&db, e(new.id)).await;
    assert_eq!(
        edges,
        vec![(
            EdgeType::Supersedes,
            EdgeOrigin::System,
            Some(e(old.id)),
            None
        )]
    );
    let spaces = all_spaces(&db).await;
    let sys = db.outgoing(e(new.id), &spaces).await.unwrap().remove(0);
    assert_eq!(sys.dst_space, Some(k));
    // системное ребро не удаляется вручную
    assert!(matches!(
        db.remove_edge(sys.id, &human()).await,
        Err(StoreError::BadInput(_))
    ));
    assert_eq!(
        backlink_srcs(&db, e(old.id), &spaces).await,
        vec![e(new.id)]
    );
    // links insight → explicit-рёбра в транзакции создания
    let target = note(&db, k, "цель", "текст\n").await;
    let mut with_links = insight(k, "со связями", None);
    with_links.links = vec![
        (EdgeType::DerivedFrom, n(target)),
        (EdgeType::RelatesTo, e(old.id)),
    ];
    let li = create_entry(&db, with_links, &agent()).await.unwrap().0;
    let mut got = out(&db, e(li.id)).await;
    got.sort_by_key(|x| x.0.as_str());
    assert_eq!(
        got,
        vec![
            (
                EdgeType::DerivedFrom,
                EdgeOrigin::Explicit,
                Some(n(target)),
                None
            ),
            (
                EdgeType::RelatesTo,
                EdgeOrigin::Explicit,
                Some(e(old.id)),
                None
            ),
        ]
    );
    // цель links не найдена — запись не создаётся
    let mut bad = insight(k, "битая связь", None);
    bad.links = vec![(EdgeType::RelatesTo, n(Id::new()))];
    assert!(matches!(
        create_entry(&db, bad, &agent()).await,
        Err(StoreError::NotFound)
    ));
}

#[tokio::test]
async fn backfill_existing_notes_on_open() {
    let dd = datadir("backfill");
    let user = Id::new();
    let path = dd.users_dir().join(format!("{user}.db"));
    let (k, a, b, old, new) = (Id::new(), Id::new(), Id::new(), Id::new(), Id::new());
    {
        // база v9 (до рёбер): заметки и замещённый insight записаны напрямую
        let db = Db::open(&path, &USER_MIGRATIONS[..9]).unwrap();
        db.write(move |tx| {
            let now = zam_store::time::fmt_ts(Timestamp::now());
            tx.execute(
                "INSERT INTO spaces (id, kind, name, created_at) VALUES (?1, 'knowledge', 'Knowledge', ?2)",
                rusqlite::params![k, now],
            )?;
            for (id, p, text, deleted) in [
                (a, "a", "[[b]] и [[нет]]\n", None),
                (b, "b", "---\nrelations: [\"depends_on: [[a]]\"]\n---\nb\n", None),
            ] {
                tx.execute(
                    "INSERT INTO nodes (id, space_id, kind, name, path, current_rev,
                                        index_state, created_at, updated_at, deleted_at)
                     VALUES (?1, ?2, 'note', ?3, ?3, 1, 'pending', ?4, ?4, ?5)",
                    rusqlite::params![id, k, p, now, deleted as Option<String>],
                )?;
                tx.execute(
                    "INSERT INTO revisions (node_id, rev, content, content_hash, size,
                                            author_kind, ts)
                     VALUES (?1, 1, ?2, 'h', 1, 'agent', ?3)",
                    rusqlite::params![id, text, now],
                )?;
            }
            // superseded_by_id старой ссылается на новую — она ставится после вставки обеих
            for (id, status, sup, by) in [
                (old, "superseded", None, None::<Id>),
                (new, "active", Some(old), None),
            ] {
                tx.execute(
                    "INSERT INTO entries (id, space_id, kind, title, title_norm, status,
                                          supersedes_id, superseded_by_id, author_kind,
                                          created_at, updated_at)
                     VALUES (?1, ?2, 'insight', 't', 't', ?3, ?4, ?5, 'agent', ?6, ?6)",
                    rusqlite::params![id, k, status, sup, by, now],
                )?;
            }
            tx.execute(
                "UPDATE entries SET superseded_by_id = ?2 WHERE id = ?1",
                rusqlite::params![old, new],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    }
    let dbs = UserDbs::new(dd.users_dir());
    let db = dbs.get(user).await.unwrap();
    // дозаполнение идёт в фоне: get его не ждёт
    dbs.wait_rechunk(user).await;
    assert_eq!(dst_of(&db, n(a), "[[b]]").await, Some(n(b)));
    assert_eq!(dst_of(&db, n(a), "[[нет]]").await, None);
    assert_eq!(
        out(&db, n(b)).await,
        vec![(
            EdgeType::DependsOn,
            EdgeOrigin::Frontmatter,
            Some(n(a)),
            Some("[[a]]".into())
        )]
    );
    assert_eq!(
        out(&db, e(new)).await,
        vec![(EdgeType::Supersedes, EdgeOrigin::System, Some(e(old)), None)]
    );
    let flag: i64 = db
        .read(|c| {
            Ok(c.query_row(
                "SELECT count(*) FROM meta WHERE key = 'edges_backfill'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(flag, 0);
    // повторное открытие ничего не дублирует
    drop(db);
    drop(dbs);
    let dbs = UserDbs::new(dd.users_dir());
    let db = dbs.get(user).await.unwrap();
    dbs.wait_rechunk(user).await;
    assert_eq!(out(&db, n(a)).await.len(), 2);
    assert_eq!(out(&db, e(new)).await.len(), 1);
}

/// Выведенные рёбра удалённого источника не перепроверяются при изменениях дерева
/// (финальная волна M3, остаток задачи 19b): корзина не увеличивает работу и запись
/// `tree_changed`, а восстановление пересобирает рёбра источника заново.
#[tokio::test]
async fn deleted_source_edges_skipped_by_tree_changed() {
    let (db, k) = setup("deleted-source-skip").await;
    let a = note(&db, k, "a", "[[Позже]]\n").await;
    delete_node(&db, a, AuthorKind::Agent).await.unwrap();
    let raw = move |db: Db| async move {
        db.read(move |c| {
            Ok(c.query_row(
                "SELECT dst_id FROM edges WHERE src_kind = 'node' AND src_id = ?1
                   AND dst_raw = '[[Позже]]' AND deleted_at IS NULL",
                [a],
                |r| r.get::<_, Option<Id>>(0),
            )?)
        })
        .await
        .unwrap()
    };
    let later = note(&db, k, "x/Позже", "текст\n").await;
    assert_eq!(
        raw(db.clone()).await,
        None,
        "ребро удалённого источника не тронуто"
    );
    restore_node(&db, a, AuthorKind::Agent).await.unwrap();
    assert_eq!(dst_of(&db, n(a), "[[Позже]]").await, Some(n(later)));
}

/// Подграф ограничен [`GRAPH_MAX_NODES`] узлами и [`GRAPH_MAX_EDGES`] рёбрами с признаком
/// `truncated` (финальная волна M3, ревью задачи 10): узел-хаб не отдаёт весь граф.
#[tokio::test]
async fn neighbors_capped_with_truncated() {
    use zam_store::{GRAPH_MAX_EDGES, GRAPH_MAX_NODES};
    let (db, k) = setup("neighbors-cap").await;
    let hub = note(&db, k, "хаб", "хаб\n").await;
    let small = note(&db, k, "малый", "[[хаб]]\n").await;
    let spaces = all_spaces(&db).await;
    let g = db.neighbors(&[n(small)], 1, &[], &spaces).await.unwrap();
    assert_eq!((g.nodes.len(), g.edges.len(), g.truncated), (2, 1, false));
    // хаб: связи с GRAPH_MAX_NODES + 50 заметками и GRAPH_MAX_EDGES + 50 повторными рёбрами
    // к первой из них (прямой вставкой: тысячи записей заметок на HDD — минуты)
    const TS: &str = "2026-10-26T00:00:00Z";
    let extra = GRAPH_MAX_NODES + 50;
    let dups = GRAPH_MAX_EDGES + 50;
    db.write(move |tx| {
        let mut first = None;
        for i in 0..extra {
            let id = Id::new();
            first.get_or_insert(id);
            tx.execute(
                "INSERT INTO nodes (id, space_id, parent_id, kind, name, path, index_state,
                                    created_at, updated_at)
                 VALUES (?1, ?2, NULL, 'note', ?3, ?3, 'excluded', ?4, ?4)",
                rusqlite::params![id, k, format!("лист{i}"), TS],
            )?;
            tx.execute(
                "INSERT INTO edges (id, space_id, src_kind, src_id, dst_kind, dst_id,
                                    dst_space_id, type, origin, created_at)
                 VALUES (?1, ?2, 'node', ?3, 'node', ?4, ?2, 'links_to', 'wikilink', ?5)",
                rusqlite::params![Id::new(), k, hub, id, TS],
            )?;
        }
        for _ in 0..dups {
            tx.execute(
                "INSERT INTO edges (id, space_id, src_kind, src_id, dst_kind, dst_id,
                                    dst_space_id, type, origin, created_at)
                 VALUES (?1, ?2, 'node', ?3, 'node', ?4, ?2, 'links_to', 'wikilink', ?5)",
                rusqlite::params![Id::new(), k, hub, first.unwrap(), TS],
            )?;
        }
        Ok(())
    })
    .await
    .unwrap();
    let g = db.neighbors(&[n(hub)], 1, &[], &spaces).await.unwrap();
    assert!(g.truncated);
    assert_eq!(g.nodes.len(), GRAPH_MAX_NODES);
    assert_eq!(g.nodes[0].r, n(hub), "начало обхода — первым");
    assert_eq!(g.edges.len(), GRAPH_MAX_EDGES);
}
