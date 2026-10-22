//! Общий набор интеграционных тестов [`VectorIndex`] (задачи 5 и 16 M2): одни и те же
//! проверки контракта прогоняются для managed Qdrant (`tests/qdrant.rs`) и для прототипа
//! на Qdrant Edge (`tests/edge.rs`, feature `edge`). Каждый случай получает свежий пустой
//! индекс; тесты, специфичные для процесса Qdrant (ключ, недоступность, падение), остаются
//! в `tests/qdrant.rs`.

use zam_core::{Id, TargetKind, TargetRef};
use zam_index::{IndexError, IndexPoint, PointPayload, VectorFilter, VectorIndex, VectorScope};

pub const DIM: usize = 16;
pub const MODEL: &str = "test/minilm@abc123";

/// Детерминированный псевдослучайный единичный вектор (xorshift).
pub fn vec_for(seed: u64) -> Vec<f32> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let v: Vec<f32> = (0..DIM)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            ((x % 2000) as f32 / 1000.0) - 1.0
        })
        .collect();
    let n = v.iter().map(|a| a * a).sum::<f32>().sqrt();
    v.into_iter().map(|a| a / n).collect()
}

pub fn sparse_for(seed: u64) -> Vec<(u32, f32)> {
    vec![((seed % 1000) as u32, 0.9), (1000 + (seed % 7) as u32, 0.3)]
}

/// Предки пути: "a/b/c" → ["a", "a/b", "a/b/c"].
pub fn ancestors(path: &str) -> Vec<String> {
    let parts: Vec<&str> = path.split('/').collect();
    (1..=parts.len()).map(|i| parts[..i].join("/")).collect()
}

pub struct Ctx {
    pub user: Id,
    pub space: Id,
}

pub fn point(
    ctx: &Ctx,
    node: Id,
    chunk_id: &str,
    seed: u64,
    path: &str,
    level: &str,
) -> IndexPoint {
    IndexPoint {
        chunk_id: chunk_id.to_string(),
        dense: vec_for(seed),
        sparse: sparse_for(seed),
        payload: PointPayload {
            user_id: ctx.user,
            space_id: ctx.space,
            space_kind: "knowledge".into(),
            solution_guid: None,
            node_id: Some(node),
            entry_id: None,
            path_ancestors: ancestors(path),
            area: None,
            level: level.into(),
            kind: "note".into(),
            tags: vec!["t".into()],
            updated_at: 1_760_000_000,
            rev: 1,
            text_hash: format!("h{seed}"),
            status: None,
            insight_kind: None,
            certainty: None,
        },
    }
}

/// Точка записи журнала `entry` (вид `kind`) вместо узла.
pub fn entry_point(ctx: &Ctx, entry: Id, chunk_id: &str, seed: u64, kind: &str) -> IndexPoint {
    let mut p = point(ctx, entry, chunk_id, seed, "j", "chunk");
    p.payload.node_id = None;
    p.payload.entry_id = Some(entry);
    p.payload.kind = kind.into();
    p
}

pub fn node_ref(id: Id) -> TargetRef {
    TargetRef {
        kind: TargetKind::Node,
        id,
    }
}

pub fn entry_ref(id: Id) -> TargetRef {
    TargetRef {
        kind: TargetKind::Entry,
        id,
    }
}

/// Фильтр одной области: пользователь и его пространство.
pub fn filter(ctx: &Ctx) -> VectorFilter {
    VectorFilter {
        scopes: vec![VectorScope {
            user_id: ctx.user,
            space_ids: vec![ctx.space],
        }],
        ..VectorFilter::default()
    }
}

/// Отсортированные `chunk_id` выдачи гибридного поиска.
async fn search_ids(idx: &dyn VectorIndex, seed: u64, f: &VectorFilter) -> Vec<String> {
    let mut ids: Vec<String> = idx
        .search(&vec_for(seed), &sparse_for(seed), f)
        .await
        .unwrap()
        .into_iter()
        .map(|h| h.chunk_id)
        .collect();
    ids.sort();
    ids
}

/// Отсортированные `chunk_id` выдачи `nearest_dense`.
async fn nearest_ids(idx: &dyn VectorIndex, seed: u64, f: &VectorFilter) -> Vec<String> {
    let mut ids: Vec<String> = idx
        .nearest_dense(&vec_for(seed), f, 100)
        .await
        .unwrap()
        .into_iter()
        .map(|h| h.chunk_id)
        .collect();
    ids.sort();
    ids
}

pub async fn ensure_creates_collection_and_is_idempotent(idx: &dyn VectorIndex) {
    idx.health().await.expect("health");
    let m1 = idx.ensure(MODEL, DIM).await.expect("ensure 1");
    assert!(m1.created);
    assert_eq!(m1.model_id.as_deref(), Some(MODEL));
    assert_eq!(m1.dim, DIM);
    assert_eq!(m1.points, 0);
    let m2 = idx.ensure(MODEL, DIM).await.expect("ensure 2");
    assert!(!m2.created);
    assert_eq!(m2.model_id.as_deref(), Some(MODEL));
    assert_eq!(m2.dim, DIM);
}

pub async fn ensure_with_other_model_returns_existing_meta_untouched(idx: &dyn VectorIndex) {
    idx.ensure(MODEL, DIM).await.unwrap();
    let ctx = Ctx {
        user: Id::new(),
        space: Id::new(),
    };
    idx.upsert(vec![point(&ctx, Id::new(), "c1", 1, "a", "chunk")])
        .await
        .unwrap();
    let m = idx
        .ensure("other/model@zzz", 32)
        .await
        .expect("ensure other");
    assert!(!m.created);
    assert_eq!(m.model_id.as_deref(), Some(MODEL));
    assert_eq!(m.dim, DIM);
    assert_eq!(m.points, 1);
}

pub async fn upsert_200_and_search_finds_itself_first(idx: &dyn VectorIndex) {
    idx.ensure(MODEL, DIM).await.unwrap();
    let ctx = Ctx {
        user: Id::new(),
        space: Id::new(),
    };
    let node = Id::new();
    let pts: Vec<IndexPoint> = (0..200)
        .map(|i| point(&ctx, node, &format!("chunk-{i}"), i, "a/b", "chunk"))
        .collect();
    idx.upsert(pts).await.unwrap();
    assert_eq!(idx.ensure(MODEL, DIM).await.unwrap().points, 200);

    let hits = idx
        .search(&vec_for(77), &sparse_for(77), &filter(&ctx))
        .await
        .unwrap();
    assert!(!hits.is_empty());
    assert!(hits.len() <= 60);
    assert_eq!(hits[0].chunk_id, "chunk-77");
    assert_eq!(hits[0].target, node_ref(node));
    assert_eq!(hits[0].user_id, ctx.user);
    assert_eq!(hits[0].level, "chunk");
}

pub async fn upsert_same_chunk_id_does_not_duplicate(idx: &dyn VectorIndex) {
    idx.ensure(MODEL, DIM).await.unwrap();
    let ctx = Ctx {
        user: Id::new(),
        space: Id::new(),
    };
    let node = Id::new();
    for round in 0..3 {
        idx.upsert(vec![
            point(&ctx, node, "c-a", round, "a", "chunk"),
            point(&ctx, node, "c-b", round + 10, "a", "chunk"),
        ])
        .await
        .unwrap();
    }
    assert_eq!(idx.ensure(MODEL, DIM).await.unwrap().points, 2);
}

pub async fn filters_isolate_users_and_spaces(idx: &dyn VectorIndex) {
    idx.ensure(MODEL, DIM).await.unwrap();
    let a = Ctx {
        user: Id::new(),
        space: Id::new(),
    };
    let b = Ctx {
        user: Id::new(),
        space: Id::new(),
    };
    let a2 = Ctx {
        user: a.user,
        space: Id::new(),
    };
    let (na, nb, na2) = (Id::new(), Id::new(), Id::new());
    // Одинаковый вектор у всех: результат определяет только фильтр.
    idx.upsert(vec![
        point(&a, na, "a-1", 5, "x", "chunk"),
        point(&b, nb, "b-1", 5, "x", "chunk"),
        point(&a2, na2, "a2-1", 5, "x", "chunk"),
    ])
    .await
    .unwrap();

    let hits = idx
        .search(&vec_for(5), &sparse_for(5), &filter(&a))
        .await
        .unwrap();
    assert_eq!(
        hits.iter().map(|h| h.chunk_id.as_str()).collect::<Vec<_>>(),
        ["a-1"]
    );

    let hits = idx
        .search(&vec_for(5), &sparse_for(5), &filter(&b))
        .await
        .unwrap();
    assert_eq!(
        hits.iter().map(|h| h.chunk_id.as_str()).collect::<Vec<_>>(),
        ["b-1"]
    );

    // Оба пространства пользователя A — match any; чужой пользователь не виден.
    let mut f = filter(&a);
    f.scopes[0].space_ids = vec![a.space, a2.space, b.space];
    let mut ids: Vec<String> = idx
        .search(&vec_for(5), &sparse_for(5), &f)
        .await
        .unwrap()
        .into_iter()
        .map(|h| h.chunk_id)
        .collect();
    ids.sort();
    assert_eq!(ids, ["a-1", "a2-1"]);
}

pub async fn path_prefix_uses_path_ancestors(idx: &dyn VectorIndex) {
    idx.ensure(MODEL, DIM).await.unwrap();
    let ctx = Ctx {
        user: Id::new(),
        space: Id::new(),
    };
    idx.upsert(vec![
        point(&ctx, Id::new(), "p1", 1, "proj/docs/a", "chunk"),
        point(&ctx, Id::new(), "p2", 2, "proj/docs", "chunk"),
        point(&ctx, Id::new(), "p3", 3, "proj/src/main", "chunk"),
        // пробел в пути: должен оставаться keyword, а не полнотекстовым match
        point(&ctx, Id::new(), "p4", 4, "my notes/x", "chunk"),
        // «proj/doc» — префикс строки, но не предок пути
        point(&ctx, Id::new(), "p5", 5, "proj/doc", "chunk"),
    ])
    .await
    .unwrap();

    let mut f = filter(&ctx);
    f.path_prefix = Some("proj/docs".into());
    let mut ids: Vec<String> = idx
        .search(&vec_for(1), &sparse_for(1), &f)
        .await
        .unwrap()
        .into_iter()
        .map(|h| h.chunk_id)
        .collect();
    ids.sort();
    assert_eq!(ids, ["p1", "p2"]);

    f.path_prefix = Some("my notes".into());
    let ids: Vec<String> = idx
        .search(&vec_for(4), &sparse_for(4), &f)
        .await
        .unwrap()
        .into_iter()
        .map(|h| h.chunk_id)
        .collect();
    assert_eq!(ids, ["p4"]);
}

pub async fn levels_filter_and_note_prefetch(idx: &dyn VectorIndex) {
    idx.ensure(MODEL, DIM).await.unwrap();
    let ctx = Ctx {
        user: Id::new(),
        space: Id::new(),
    };
    let node = Id::new();
    idx.upsert(vec![
        point(&ctx, node, "n-note", 9, "a", "note"),
        point(&ctx, node, "n-sec", 9, "a", "section"),
        point(&ctx, node, "n-chunk", 9, "a", "chunk"),
    ])
    .await
    .unwrap();

    // Без ограничения уровней: chunk через dense+sparse, note/section через третий prefetch.
    let mut ids: Vec<String> = idx
        .search(&vec_for(9), &sparse_for(9), &filter(&ctx))
        .await
        .unwrap()
        .into_iter()
        .map(|h| h.chunk_id)
        .collect();
    ids.sort();
    assert_eq!(ids, ["n-chunk", "n-note", "n-sec"]);

    let mut f = filter(&ctx);
    f.levels = vec!["note".into()];
    let hits = idx.search(&vec_for(9), &sparse_for(9), &f).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].chunk_id, "n-note");
    assert_eq!(hits[0].level, "note");
}

/// Точки папок (канал папок, M3): только с `folders`, своим фильтром — виды и `certainty`
/// запроса к ним не применяются, области и префикс пути — применяются.
pub async fn folders_prefetch_on_request(idx: &dyn VectorIndex) {
    idx.ensure(MODEL, DIM).await.unwrap();
    let ctx = Ctx {
        user: Id::new(),
        space: Id::new(),
    };
    let (node, folder) = (Id::new(), Id::new());
    let mut f_pt = point(&ctx, folder, "f-ops", 11, "ops", "folder");
    f_pt.payload.kind = "folder".into();
    idx.upsert(vec![point(&ctx, node, "c-ops", 11, "ops/a", "chunk"), f_pt])
        .await
        .unwrap();
    let mut f = filter(&ctx);
    f.kinds = vec!["note".into()];
    assert_eq!(search_ids(idx, 11, &f).await, ["c-ops"]);
    f.folders = true;
    f.certainty = vec!["axiom".into()];
    // чанк без certainty отфильтрован, папка — нет
    let hits = idx.search(&vec_for(11), &sparse_for(11), &f).await.unwrap();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(
        (hits[0].chunk_id.as_str(), hits[0].level.as_str()),
        ("f-ops", "folder")
    );
    assert_eq!(hits[0].target, node_ref(folder));
    f.certainty.clear();
    assert_eq!(search_ids(idx, 11, &f).await, ["c-ops", "f-ops"]);
    // префикс пути и области — и для папок
    f.path_prefix = Some("other".into());
    assert!(search_ids(idx, 11, &f).await.is_empty());
    f.path_prefix = None;
    f.scopes[0].space_ids = vec![Id::new()];
    assert!(search_ids(idx, 11, &f).await.is_empty());
}

pub async fn delete_node_removes_all_its_points(idx: &dyn VectorIndex) {
    idx.ensure(MODEL, DIM).await.unwrap();
    let ctx = Ctx {
        user: Id::new(),
        space: Id::new(),
    };
    let (n1, n2) = (Id::new(), Id::new());
    let mut pts: Vec<IndexPoint> = (0..5)
        .map(|i| point(&ctx, n1, &format!("n1-{i}"), i, "a", "chunk"))
        .collect();
    pts.push(point(&ctx, n2, "n2-0", 100, "a", "chunk"));
    idx.upsert(pts).await.unwrap();
    // Чужой пользователь с тем же node_id не удаляет чужие точки.
    idx.delete_node(Id::new(), n1).await.unwrap();
    assert_eq!(idx.ensure(MODEL, DIM).await.unwrap().points, 6);

    idx.delete_node(ctx.user, n1).await.unwrap();
    assert_eq!(idx.ensure(MODEL, DIM).await.unwrap().points, 1);
    let hits = idx
        .search(&vec_for(2), &sparse_for(2), &filter(&ctx))
        .await
        .unwrap();
    assert!(hits.iter().all(|h| h.target == node_ref(n2)));
}

pub async fn missing_collection_is_not_found_and_count_filters(idx: &dyn VectorIndex) {
    let ctx = Ctx {
        user: Id::new(),
        space: Id::new(),
    };
    // Коллекции нет: upsert и delete — NotFound (воркер вызывает ensure и повторяет).
    let err = idx
        .upsert(vec![point(&ctx, Id::new(), "c1", 1, "a", "chunk")])
        .await
        .unwrap_err();
    assert!(matches!(err, IndexError::NotFound(_)), "{err:?}");
    let err = idx.delete_node(ctx.user, Id::new()).await.unwrap_err();
    assert!(matches!(err, IndexError::NotFound(_)), "{err:?}");

    idx.ensure(MODEL, DIM).await.unwrap();
    let other = Ctx {
        user: ctx.user,
        space: Id::new(),
    };
    idx.upsert(vec![
        point(&ctx, Id::new(), "c1", 1, "a/b", "chunk"),
        point(&ctx, Id::new(), "c2", 2, "a/b", "note"),
        point(&other, Id::new(), "c3", 3, "x", "chunk"),
    ])
    .await
    .unwrap();
    assert_eq!(idx.count(&filter(&ctx)).await.unwrap(), 2);
    let mut f = filter(&ctx);
    f.levels = vec!["note".into()];
    assert_eq!(idx.count(&f).await.unwrap(), 1);
    f = filter(&other);
    f.path_prefix = Some("a".into());
    assert_eq!(idx.count(&f).await.unwrap(), 0);
    f.scopes[0].space_ids = vec![ctx.space, other.space];
    f.path_prefix = None;
    assert_eq!(idx.count(&f).await.unwrap(), 3);
    f.scopes[0].user_id = Id::new();
    assert_eq!(idx.count(&f).await.unwrap(), 0);
    // область без пространств и пустой список областей не пропускают ничего
    f = filter(&ctx);
    f.scopes[0].space_ids.clear();
    assert_eq!(idx.count(&f).await.unwrap(), 0);
    f.scopes.clear();
    assert_eq!(idx.count(&f).await.unwrap(), 0);
}

pub async fn human_space_is_rejected(idx: &dyn VectorIndex) {
    idx.ensure(MODEL, DIM).await.unwrap();
    let ctx = Ctx {
        user: Id::new(),
        space: Id::new(),
    };
    let mut p = point(&ctx, Id::new(), "h1", 1, "a", "chunk");
    p.payload.space_kind = "Human".into();
    let err = idx.upsert(vec![p]).await.unwrap_err();
    assert!(matches!(err, IndexError::Other(_)), "{err:?}");
    assert_eq!(idx.ensure(MODEL, DIM).await.unwrap().points, 0);
}

pub async fn drop_all_removes_collection(idx: &dyn VectorIndex) {
    idx.ensure(MODEL, DIM).await.unwrap();
    idx.drop_all().await.unwrap();
    // повторно — без ошибки
    idx.drop_all().await.unwrap();
    let m = idx.ensure("new/model@1", 8).await.unwrap();
    assert!(m.created);
    assert_eq!(m.model_id.as_deref(), Some("new/model@1"));
    assert_eq!(m.dim, 8);
}

/// Дизъюнкция областей: «B и его Knowledge» ИЛИ «A и его Solution» — Knowledge A не виден.
pub async fn scopes_disjunction_isolates_users_and_spaces(idx: &dyn VectorIndex) {
    idx.ensure(MODEL, DIM).await.unwrap();
    let a_know = Ctx {
        user: Id::new(),
        space: Id::new(),
    };
    let a_sol = Ctx {
        user: a_know.user,
        space: Id::new(),
    };
    let b_know = Ctx {
        user: Id::new(),
        space: Id::new(),
    };
    idx.upsert(vec![
        point(&a_know, Id::new(), "ak", 5, "p/x", "chunk"),
        point(&a_sol, Id::new(), "as", 5, "p/x", "chunk"),
        point(&a_sol, Id::new(), "as-other", 5, "q", "chunk"),
        point(&b_know, Id::new(), "bk", 5, "p/x", "chunk"),
    ])
    .await
    .unwrap();
    let mut f = VectorFilter {
        scopes: vec![
            VectorScope {
                user_id: b_know.user,
                space_ids: vec![b_know.space],
            },
            VectorScope {
                user_id: a_know.user,
                space_ids: vec![a_sol.space],
            },
        ],
        ..VectorFilter::default()
    };
    assert_eq!(search_ids(idx, 5, &f).await, ["as", "as-other", "bk"]);
    assert_eq!(nearest_ids(idx, 5, &f).await, ["as", "as-other", "bk"]);
    assert_eq!(idx.count(&f).await.unwrap(), 3);
    // выдача несёт владельца базы
    let hits = idx.nearest_dense(&vec_for(5), &f, 10).await.unwrap();
    for h in &hits {
        let owner = if h.chunk_id == "bk" {
            b_know.user
        } else {
            a_know.user
        };
        assert_eq!(h.user_id, owner, "{h:?}");
    }
    // префикс пути — внутри каждой области
    f.path_prefix = Some("p".into());
    assert_eq!(search_ids(idx, 5, &f).await, ["as", "bk"]);
    // пространство A под тенантом B — не область B
    let cross = VectorFilter {
        scopes: vec![VectorScope {
            user_id: b_know.user,
            space_ids: vec![a_know.space],
        }],
        ..VectorFilter::default()
    };
    assert!(search_ids(idx, 5, &cross).await.is_empty());
    assert_eq!(idx.count(&cross).await.unwrap(), 0);
    // пустой список областей — пустая выдача, удаление области ничего не трогает
    let none = VectorFilter::default();
    assert!(search_ids(idx, 5, &none).await.is_empty());
    assert!(nearest_ids(idx, 5, &none).await.is_empty());
    idx.delete_scope(&none).await.unwrap();
    assert_eq!(idx.ensure(MODEL, DIM).await.unwrap().points, 4);
    // удаление по дизъюнкции областей — только их точки
    f.path_prefix = None;
    idx.delete_scope(&f).await.unwrap();
    assert_eq!(idx.ensure(MODEL, DIM).await.unwrap().points, 1);
    assert_eq!(search_ids(idx, 5, &filter(&a_know)).await, ["ak"]);
}

/// Исключающий фильтр статуса: точки M2 без `status` проходят.
pub async fn exclude_statuses_keeps_points_without_status(idx: &dyn VectorIndex) {
    idx.ensure(MODEL, DIM).await.unwrap();
    let ctx = Ctx {
        user: Id::new(),
        space: Id::new(),
    };
    let mut pts = vec![point(&ctx, Id::new(), "m2", 3, "a", "chunk")];
    for st in ["active", "revisit", "superseded", "retracted"] {
        let mut p = entry_point(&ctx, Id::new(), st, 3, "insight");
        p.payload.status = Some(st.into());
        pts.push(p);
    }
    idx.upsert(pts).await.unwrap();
    let mut f = filter(&ctx);
    f.exclude_statuses = vec!["superseded".into(), "retracted".into()];
    assert_eq!(search_ids(idx, 3, &f).await, ["active", "m2", "revisit"]);
    assert_eq!(nearest_ids(idx, 3, &f).await, ["active", "m2", "revisit"]);
    assert_eq!(idx.count(&f).await.unwrap(), 3);
    // без фильтра — все
    assert_eq!(idx.count(&filter(&ctx)).await.unwrap(), 5);
}

/// Фильтры вида, вида инсайта, `certainty` и области (`area`).
pub async fn kinds_insight_kinds_certainty_area_filters(idx: &dyn VectorIndex) {
    idx.ensure(MODEL, DIM).await.unwrap();
    let ctx = Ctx {
        user: Id::new(),
        space: Id::new(),
    };
    let note = point(&ctx, Id::new(), "note", 4, "a", "chunk");
    let mut ax = entry_point(&ctx, Id::new(), "ins-axiom", 4, "insight");
    ax.payload.insight_kind = Some("pattern".into());
    ax.payload.certainty = Some("axiom".into());
    ax.payload.area = Some("backend".into());
    let mut hy = entry_point(&ctx, Id::new(), "ins-hypo", 4, "insight");
    hy.payload.insight_kind = Some("gotcha".into());
    hy.payload.certainty = Some("hypothesis".into());
    hy.payload.area = Some("frontend".into());
    let mut bare = entry_point(&ctx, Id::new(), "ins-bare", 4, "insight");
    bare.payload.insight_kind = Some("gotcha".into());
    let log = entry_point(&ctx, Id::new(), "changelog", 4, "changelog");
    idx.upsert(vec![note, ax, hy, bare, log]).await.unwrap();

    let all = ["changelog", "ins-axiom", "ins-bare", "ins-hypo", "note"];
    assert_eq!(search_ids(idx, 4, &filter(&ctx)).await, all);
    let mut f = filter(&ctx);
    f.kinds = vec!["insight".into()];
    assert_eq!(
        search_ids(idx, 4, &f).await,
        ["ins-axiom", "ins-bare", "ins-hypo"]
    );
    f.kinds = vec!["note".into(), "changelog".into()];
    assert_eq!(nearest_ids(idx, 4, &f).await, ["changelog", "note"]);
    f = filter(&ctx);
    f.insight_kinds = vec!["gotcha".into()];
    assert_eq!(search_ids(idx, 4, &f).await, ["ins-bare", "ins-hypo"]);
    f = filter(&ctx);
    f.certainty = vec!["axiom".into(), "theorem".into()];
    assert_eq!(search_ids(idx, 4, &f).await, ["ins-axiom"]);
    assert_eq!(idx.count(&f).await.unwrap(), 1);
    f = filter(&ctx);
    f.area = Some("frontend".into());
    assert_eq!(nearest_ids(idx, 4, &f).await, ["ins-hypo"]);
    // сочетание условий — конъюнкция
    f.kinds = vec!["insight".into()];
    f.certainty = vec!["axiom".into()];
    assert!(search_ids(idx, 4, &f).await.is_empty());
}

/// Удаление по узлам и записям — только тенанта `user`.
pub async fn delete_targets_by_node_and_entry(idx: &dyn VectorIndex) {
    idx.ensure(MODEL, DIM).await.unwrap();
    let ctx = Ctx {
        user: Id::new(),
        space: Id::new(),
    };
    let (n1, e1, e2) = (Id::new(), Id::new(), Id::new());
    idx.upsert(vec![
        point(&ctx, n1, "n1-a", 1, "a", "chunk"),
        point(&ctx, n1, "n1-b", 2, "a", "note"),
        entry_point(&ctx, e1, "e1-a", 3, "insight"),
        entry_point(&ctx, e1, "e1-b", 4, "insight"),
        entry_point(&ctx, e2, "e2-a", 5, "changelog"),
    ])
    .await
    .unwrap();
    // пустой список — ничего; чужой тенант с теми же id — ничего
    idx.delete_targets(ctx.user, &[]).await.unwrap();
    idx.delete_targets(Id::new(), &[node_ref(n1), entry_ref(e1)])
        .await
        .unwrap();
    assert_eq!(idx.ensure(MODEL, DIM).await.unwrap().points, 5);
    // id узла как id записи (и наоборот) — не та цель
    idx.delete_targets(ctx.user, &[entry_ref(n1), node_ref(e1)])
        .await
        .unwrap();
    assert_eq!(idx.ensure(MODEL, DIM).await.unwrap().points, 5);

    idx.delete_targets(ctx.user, &[node_ref(n1), entry_ref(e1)])
        .await
        .unwrap();
    assert_eq!(idx.ensure(MODEL, DIM).await.unwrap().points, 1);
    let hits = idx
        .search(&vec_for(5), &sparse_for(5), &filter(&ctx))
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].target, entry_ref(e2));
}

/// `nearest_dense`: score — косинус, порядок по убыванию, предел и фильтр соблюдаются.
pub async fn nearest_dense_returns_cosine(idx: &dyn VectorIndex) {
    idx.ensure(MODEL, DIM).await.unwrap();
    let ctx = Ctx {
        user: Id::new(),
        space: Id::new(),
    };
    let other = Ctx {
        user: Id::new(),
        space: ctx.space,
    };
    let e = Id::new();
    let mut pts: Vec<IndexPoint> = (1..=6)
        .map(|i| point(&ctx, Id::new(), &format!("c{i}"), i, "a", "chunk"))
        .collect();
    pts.push(entry_point(&ctx, e, "entry", 1, "insight"));
    pts.push(point(&other, Id::new(), "foreign", 1, "a", "chunk"));
    idx.upsert(pts).await.unwrap();

    let q = vec_for(1);
    let hits = idx.nearest_dense(&q, &filter(&ctx), 4).await.unwrap();
    assert_eq!(hits.len(), 4);
    assert!(hits.iter().all(|h| h.chunk_id != "foreign"), "{hits:?}");
    assert!(
        hits.windows(2).all(|w| w[0].score >= w[1].score),
        "{hits:?}"
    );
    let cos = |seed: u64| -> f32 { q.iter().zip(vec_for(seed)).map(|(a, b)| a * b).sum() };
    for h in &hits {
        let expected = match h.chunk_id.as_str() {
            "entry" => {
                assert_eq!(h.target, entry_ref(e));
                1.0
            }
            id => cos(id[1..].parse().unwrap()),
        };
        assert!(
            (h.score - expected).abs() < 1e-3,
            "{}: {} против {expected}",
            h.chunk_id,
            h.score
        );
    }
    // совпадающий вектор — косинус 1
    assert!((hits[0].score - 1.0).abs() < 1e-3, "{hits:?}");
    // уровни ограничивают выдачу
    let mut f = filter(&ctx);
    f.levels = vec!["note".into()];
    assert!(idx.nearest_dense(&q, &f, 4).await.unwrap().is_empty());
}

/// Точка ровно с одной целью: узел или запись.
pub async fn upsert_requires_exactly_one_target(idx: &dyn VectorIndex) {
    idx.ensure(MODEL, DIM).await.unwrap();
    let ctx = Ctx {
        user: Id::new(),
        space: Id::new(),
    };
    let mut none = point(&ctx, Id::new(), "none", 1, "a", "chunk");
    none.payload.node_id = None;
    let err = idx.upsert(vec![none]).await.unwrap_err();
    assert!(matches!(err, IndexError::Other(_)), "{err:?}");
    let mut both = point(&ctx, Id::new(), "both", 1, "a", "chunk");
    both.payload.entry_id = Some(Id::new());
    let err = idx.upsert(vec![both]).await.unwrap_err();
    assert!(matches!(err, IndexError::Other(_)), "{err:?}");
    assert_eq!(idx.ensure(MODEL, DIM).await.unwrap().points, 0);
}

/// Тест на каждый общий случай: `$make(короткое имя)` возвращает свежий индекс и охрану его
/// окружения (процесс Qdrant или каталог Edge), которая живёт до конца теста.
#[macro_export]
macro_rules! vector_index_cases {
    ($make:path) => {
        $crate::vector_index_cases!(@each $make;
            ensure_creates_collection_and_is_idempotent,
            ensure_with_other_model_returns_existing_meta_untouched,
            upsert_200_and_search_finds_itself_first,
            upsert_same_chunk_id_does_not_duplicate,
            filters_isolate_users_and_spaces,
            path_prefix_uses_path_ancestors,
            levels_filter_and_note_prefetch,
            folders_prefetch_on_request,
            delete_node_removes_all_its_points,
            missing_collection_is_not_found_and_count_filters,
            human_space_is_rejected,
            drop_all_removes_collection,
            scopes_disjunction_isolates_users_and_spaces,
            exclude_statuses_keeps_points_without_status,
            kinds_insight_kinds_certainty_area_filters,
            delete_targets_by_node_and_entry,
            nearest_dense_returns_cosine,
            upsert_requires_exactly_one_target,
        );
    };
    (@each $make:path; $($name:ident,)*) => {
        $(
            #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
            async fn $name() {
                // Короткое имя каталога: файлы сегментов Qdrant лежат глубоко, и длинное
                // имя теста выводит путь за MAX_PATH (260) на Windows.
                let name = stringify!($name);
                let (idx, _env) = $make(&name[..name.len().min(8)]);
                common::cases::$name(&*idx).await;
            }
        )*
    };
}
