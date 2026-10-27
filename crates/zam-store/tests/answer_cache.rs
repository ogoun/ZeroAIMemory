//! Кэш выжимок (§9.3–9.4, задача 7 M4): закрепление источников, попадание (точное и
//! семантическое), инвалидация в транзакциях записи, новые источники, срок жизни и миграция.
use std::path::PathBuf;

use jiff::{SignedDuration, Timestamp};
use serde_json::json;
use zam_core::{Id, TargetKind, TargetRef};
use zam_store::{
    Attribution, AuthorKind, CacheError, CachePut, CacheQuery, Certainty, DataDir, Db, EntryKind,
    EntryPatch, NewEntry, NewNote, NodePath, PromoteAs, SourceSpec, SpaceKind, StaleKind,
    StaleSource, UserDbs, cache_count, cache_get, cache_lookup, cache_promote, cache_put,
    cache_put_with, create_entry, create_folder, create_note, delete_entry, delete_node, gc_cache,
    list_spaces, mark_new_sources, move_node, normalize_query, patch_note, retract_entry,
    scope_hash, supersede_entry, update_entry, update_note,
};

/// Модель векторов запросов в тестах.
const MODEL: &str = "fake/test@00000001";

fn datadir(name: &str) -> DataDir {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("answer-cache-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    DataDir::open(&dir).unwrap()
}

struct W {
    _dd: DataDir,
    db: Db,
    user: Id,
    knowledge: Id,
    human: Id,
}

async fn world(name: &str) -> W {
    let dd = datadir(name);
    let user = Id::new();
    let db = UserDbs::new(dd.users_dir()).get(user).await.unwrap();
    let spaces = list_spaces(&db).await.unwrap();
    let find = |k: SpaceKind| spaces.iter().find(|s| s.kind == k).unwrap().id;
    W {
        knowledge: find(SpaceKind::Knowledge),
        human: find(SpaceKind::Human),
        _dd: dd,
        db,
        user,
    }
}

fn agent() -> Attribution {
    Attribution::new(AuthorKind::Agent)
}

fn node(id: Id) -> TargetRef {
    TargetRef {
        kind: TargetKind::Node,
        id,
    }
}

fn entry(id: Id) -> TargetRef {
    TargetRef {
        kind: TargetKind::Entry,
        id,
    }
}

async fn note(w: &W, space: Id, path: &str, text: &str) -> Id {
    create_note(
        &w.db,
        NewNote {
            space_id: space,
            path: NodePath::parse(path).unwrap(),
            content: text.into(),
            l0: None,
            attrib: agent(),
        },
    )
    .await
    .unwrap()
    .id
}

async fn insight(w: &W, title: &str) -> Id {
    create_entry(
        &w.db,
        NewEntry {
            space_id: w.knowledge,
            kind: EntryKind::Insight,
            title: title.into(),
            body: "# Суть\nLF в скриптах.\n".into(),
            data: json!({"kind": "gotcha"}),
            area: None,
            tags: Vec::new(),
            status: None,
            certainty: Some(Certainty::Hypothesis),
            idempotency_key: None,
            triggers: Vec::new(),
            links: Vec::new(),
        },
        &agent(),
    )
    .await
    .unwrap()
    .0
    .id
}

fn src(t: TargetRef) -> SourceSpec {
    SourceSpec {
        target: t,
        rev: None,
        section: None,
    }
}

fn scope() -> String {
    scope_hash(
        &json!({"solution": null, "include_knowledge": true, "include_rules": true,
                       "path_prefix": null, "area": null, "filters": {}}),
    )
}

fn put(w: &W, query: &str, sources: Vec<SourceSpec>) -> CachePut {
    CachePut {
        space_id: w.knowledge,
        area: None,
        author_user_id: w.user,
        author_kind: AuthorKind::Agent,
        session_id: Some("s1".into()),
        machine: Some("box".into()),
        scope_hash: scope(),
        query: query.into(),
        query_vec: None,
        model_id: Some(MODEL.into()),
        summary: "Выжимка: используйте LF.".into(),
        tokens: 6,
        sources,
    }
}

fn query(w: &W, q: &str) -> CacheQuery {
    CacheQuery {
        space_id: w.knowledge,
        scope_hash: scope(),
        query_norm: normalize_query(q),
        query_vec: None,
        model_id: Some(MODEL.into()),
        session_id: Some("s1".into()),
        cross_session: true,
        semantic_threshold: 0.92,
        limit: 2,
        visible_spaces: None,
    }
}

/// Причины выжимки после проверки.
async fn stale_of(w: &W, id: Id) -> Vec<StaleSource> {
    cache_get(&w.db, id).await.unwrap().unwrap().stale
}

fn reasons(s: &[StaleSource]) -> Vec<(TargetRef, StaleKind)> {
    s.iter().map(|s| (s.target, s.reason)).collect()
}

/// Сохранение закрепляет ревизию и хэш источников (секции — хэш текста секции); устаревшая
/// ревизия в запросе, Human, папка, нет секции — отказ без записи.
#[tokio::test]
async fn cache_put_pins_revisions() {
    let w = world("pin").await;
    let n = note(
        &w,
        w.knowledge,
        "Bash",
        "# Bash\nLF.\n## Windows\nCRLF ломает.\n",
    )
    .await;
    update_note(
        &w.db,
        n,
        1,
        "# Bash\nLF!\n## Windows\nCRLF ломает.\n".into(),
        None,
        agent(),
    )
    .await
    .unwrap();
    let i = insight(&w, "LF").await;
    let e = cache_put(
        &w.db,
        &put(
            &w,
            "  Как  писать BASH ",
            vec![
                src(node(n)),
                SourceSpec {
                    target: node(n),
                    rev: Some(2),
                    section: Some("Bash > Windows".into()),
                },
                src(entry(i)),
            ],
        ),
    )
    .await
    .unwrap();
    assert_eq!(e.state, "valid");
    assert_eq!(e.sources.len(), 3);
    assert_eq!(e.sources[0].rev, 2);
    assert_eq!(e.sources[1].section.as_deref(), Some("Bash > Windows"));
    assert_ne!(e.sources[0].text_hash, e.sources[1].text_hash);
    assert_eq!(e.sources[2].rev, 1);
    assert_eq!(normalize_query("  Как  писать BASH "), "как писать bash");
    assert!(stale_of(&w, e.id).await.is_empty());

    let err = |p: CachePut| {
        let db = w.db.clone();
        async move { cache_put(&db, &p).await.unwrap_err() }
    };
    let mut p = put(&w, "q", vec![src(node(n))]);
    p.sources[0].rev = Some(1);
    assert!(matches!(
        err(p).await,
        CacheError::SourceChanged { current_rev: 2, .. }
    ));
    let h = note(&w, w.human, "тайна", "секрет\n").await;
    assert!(matches!(
        err(put(&w, "q", vec![src(node(h))])).await,
        CacheError::ForbiddenSource(_)
    ));
    let f = create_folder(&w.db, w.knowledge, NodePath::parse("Папка").unwrap())
        .await
        .unwrap();
    assert!(matches!(
        err(put(&w, "q", vec![src(node(f.id))])).await,
        CacheError::SourceNotFound { .. }
    ));
    let mut p = put(&w, "q", vec![src(node(n))]);
    p.sources[0].section = Some("Нет такой".into());
    assert!(matches!(err(p).await, CacheError::SourceNotFound { .. }));
    assert!(matches!(
        err(put(&w, "q", vec![src(node(Id::new()))])).await,
        CacheError::SourceNotFound { .. }
    ));
    assert_eq!(cache_count(&w.db).await.unwrap(), 1);
    // тот же ключ — прежняя выжимка заменена
    let e2 = cache_put(&w.db, &put(&w, "как писать bash", vec![src(node(n))]))
        .await
        .unwrap();
    assert_eq!(cache_count(&w.db).await.unwrap(), 1);
    assert!(cache_get(&w.db, e.id).await.unwrap().is_none());
    assert!(cache_get(&w.db, e2.id).await.unwrap().is_some());
}

/// Стенд Debian 13 (доработка): секция источника разрешается, как чтение — по однозначному
/// хвосту пути; закрепляется полный путь, хвост и полный путь — один источник; несколько
/// совпадений хвоста — `SourceNotFound` (422), как у `/read`.
#[tokio::test]
async fn cache_put_section_tail_pins_full_path() {
    let w = world("pin-tail").await;
    let n = note(
        &w,
        w.knowledge,
        "Сборка",
        "# Сборка\nmake\n## Тесты\ncargo test\n# Деплой\n## Шаги\n1\n# Откат\n## Шаги\n2\n",
    )
    .await;
    let sec = |s: &str| SourceSpec {
        target: node(n),
        rev: None,
        section: Some(s.into()),
    };
    let e = cache_put(&w.db, &put(&w, "как тестировать", vec![sec("Тесты")]))
        .await
        .unwrap();
    assert_eq!(e.sources.len(), 1);
    assert_eq!(e.sources[0].section.as_deref(), Some("Сборка > Тесты"));
    let full = cache_put(
        &w.db,
        &put(&w, "как тестировать 2", vec![sec("Сборка > Тесты")]),
    )
    .await
    .unwrap();
    assert_eq!(e.sources[0].text_hash, full.sources[0].text_hash);
    assert!(stale_of(&w, e.id).await.is_empty());
    // хвост и полный путь в одной выжимке — один источник
    let both = cache_put(
        &w.db,
        &put(
            &w,
            "как тестировать 3",
            vec![sec("Тесты"), sec("Сборка > Тесты")],
        ),
    )
    .await
    .unwrap();
    assert_eq!(both.sources.len(), 1);
    // неоднозначный хвост
    let r = cache_put(&w.db, &put(&w, "шаги", vec![sec("Шаги")])).await;
    assert!(
        matches!(&r, Err(CacheError::SourceNotFound { detail, .. }) if detail.contains("неоднозначен")),
        "{r:?}"
    );
}

/// Точное попадание по `(scope_hash, query_norm)`, семантическое — по косинусу ≥ порога;
/// не больше двух, своя сессия первой; чужие — только при `cross_session`; `hits` растёт.
#[tokio::test]
async fn lookup_exact_and_semantic() {
    let w = world("lookup").await;
    let n = note(&w, w.knowledge, "Bash", "# Bash\nLF.\n").await;
    let mut a = put(&w, "Как писать bash", vec![src(node(n))]);
    a.query_vec = Some(vec![1.0, 0.0, 0.0]);
    a.session_id = Some("other".into());
    let a = cache_put(&w.db, &a).await.unwrap();
    let mut b = put(&w, "bash скрипты на windows", vec![src(node(n))]);
    b.query_vec = Some(vec![0.95, 0.31, 0.0]);
    let b = cache_put(&w.db, &b).await.unwrap();
    let mut c = put(&w, "совсем другое", vec![src(node(n))]);
    c.query_vec = Some(vec![0.0, 0.0, 1.0]);
    cache_put(&w.db, &c).await.unwrap();

    // точное — без вектора
    let hits = cache_lookup(&w.db, &query(&w, "как ПИСАТЬ bash"))
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].entry.id, a.id);
    assert!(hits[0].is_valid());
    // семантическое: своя сессия (b) первой, затем точное чужое (a); c — ниже порога
    let mut q = query(&w, "как писать bash");
    q.query_vec = Some(vec![1.0, 0.05, 0.0]);
    let hits = cache_lookup(&w.db, &q).await.unwrap();
    let ids: Vec<Id> = hits.iter().map(|h| h.entry.id).collect();
    assert_eq!(ids, vec![b.id, a.id]);
    // без чужих сессий — только своя
    q.cross_session = false;
    let hits = cache_lookup(&w.db, &q).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].entry.id, b.id);
    // другая область запроса — мимо
    let mut q = query(&w, "как писать bash");
    q.scope_hash = scope_hash(&json!({"solution": "x"}));
    assert!(cache_lookup(&w.db, &q).await.unwrap().is_empty());
    // видимость: источник вне видимых пространств — не выдаётся
    let mut q = query(&w, "как писать bash");
    q.visible_spaces = Some(vec![Id::new()]);
    assert!(cache_lookup(&w.db, &q).await.unwrap().is_empty());
    let got = cache_get(&w.db, a.id).await.unwrap().unwrap();
    assert_eq!(got.entry.hits, 2);
    assert!(got.entry.last_used_at > a.last_used_at);
}

/// Правка, PATCH чужой секции, удаление, удаление папки-предка (корзина) и перенос заметки
/// делают выжимку недействительной в той же транзакции; секция с прежним текстом остаётся.
#[tokio::test]
async fn invalidate_on_note_edit_delete_trash_move() {
    let w = world("note-inv").await;
    let text = "# Bash\nLF.\n## Windows\nCRLF.\n## Linux\nLF.\n";
    let n = note(&w, w.knowledge, "Инструменты/Bash", text).await;
    let whole = cache_put(&w.db, &put(&w, "весь", vec![src(node(n))]))
        .await
        .unwrap();
    let mut p = put(&w, "секция", vec![src(node(n))]);
    p.sources[0].section = Some("Bash > Windows".into());
    let sect = cache_put(&w.db, &p).await.unwrap();
    // правка другой секции: выжимка по целому — changed, по секции Windows — действует
    patch_note(
        &w.db,
        n,
        1,
        vec![zam_markdown::PatchOp::ReplaceSection {
            section: "Bash > Linux".into(),
            content: "LF и только LF.\n".into(),
        }],
        agent(),
    )
    .await
    .unwrap();
    let st = cache_get(&w.db, whole.id).await.unwrap().unwrap();
    assert_eq!(st.entry.state, "stale");
    assert_eq!(reasons(&st.stale), vec![(node(n), StaleKind::Changed)]);
    let s = cache_get(&w.db, sect.id).await.unwrap().unwrap();
    assert!(s.is_valid(), "{:?}", s.stale);
    assert_eq!(s.entry.sources[0].rev, 2);
    // правка секции Windows — и она недействительна
    let (_, cur) = zam_store::node_content(&w.db, n, None).await.unwrap();
    assert!(cur.contains("LF и только LF."), "{cur}");
    update_note(
        &w.db,
        n,
        2,
        cur.replace("CRLF.", "CRLF ломает."),
        None,
        agent(),
    )
    .await
    .unwrap();
    let s = cache_get(&w.db, sect.id).await.unwrap().unwrap();
    assert_eq!(s.entry.state, "stale");
    assert_eq!(s.stale[0].section.as_deref(), Some("Bash > Windows"));

    // перенос — changed
    let m = note(&w, w.knowledge, "Перенос", "# M\n").await;
    let moved = cache_put(&w.db, &put(&w, "перенос", vec![src(node(m))]))
        .await
        .unwrap();
    move_node(
        &w.db,
        m,
        NodePath::parse("Архив/Перенос").unwrap(),
        AuthorKind::Agent,
    )
    .await
    .unwrap();
    assert_eq!(
        reasons(&stale_of(&w, moved.id).await),
        vec![(node(m), StaleKind::Changed)]
    );
    // удаление — deleted
    let d = note(&w, w.knowledge, "Удалить", "# D\n").await;
    let del = cache_put(&w.db, &put(&w, "удаление", vec![src(node(d))]))
        .await
        .unwrap();
    delete_node(&w.db, d, AuthorKind::Agent).await.unwrap();
    let st = cache_get(&w.db, del.id).await.unwrap().unwrap();
    assert_eq!(st.entry.state, "stale");
    assert_eq!(reasons(&st.stale), vec![(node(d), StaleKind::Deleted)]);
    // корзина: удаление папки-предка помечает выжимку по заметке внутри
    let t = note(&w, w.knowledge, "Папка/Внутри", "# T\n").await;
    let trash = cache_put(&w.db, &put(&w, "корзина", vec![src(node(t))]))
        .await
        .unwrap();
    let folder =
        zam_store::get_node_by_path(&w.db, w.knowledge, &NodePath::parse("Папка").unwrap())
            .await
            .unwrap();
    delete_node(&w.db, folder.id, AuthorKind::Agent)
        .await
        .unwrap();
    let st = cache_get(&w.db, trash.id).await.unwrap().unwrap();
    assert_eq!(st.entry.state, "stale");
    assert_eq!(reasons(&st.stale), vec![(node(t), StaleKind::Deleted)]);
}

/// Замещение и отзыв insight — `superseded`, `retracted`; правка тела — `changed`; удаление
/// записи — `deleted`; смена статуса без смены тела выжимку не портит.
#[tokio::test]
async fn invalidate_on_entry_supersede_retract() {
    let w = world("entry-inv").await;
    let a = insight(&w, "A").await;
    let b = insight(&w, "B").await;
    let c = insight(&w, "C").await;
    let d = insight(&w, "D").await;
    let ca = cache_put(&w.db, &put(&w, "a", vec![src(entry(a))]))
        .await
        .unwrap();
    let cb = cache_put(&w.db, &put(&w, "b", vec![src(entry(b))]))
        .await
        .unwrap();
    let cc = cache_put(&w.db, &put(&w, "c", vec![src(entry(c))]))
        .await
        .unwrap();
    let cd = cache_put(&w.db, &put(&w, "d", vec![src(entry(d))]))
        .await
        .unwrap();
    let mut new = NewEntry {
        space_id: w.knowledge,
        kind: EntryKind::Insight,
        title: "A2".into(),
        body: "новое".into(),
        data: json!({"kind": "gotcha"}),
        area: None,
        tags: Vec::new(),
        status: None,
        certainty: Some(Certainty::Hypothesis),
        idempotency_key: None,
        triggers: Vec::new(),
        links: Vec::new(),
    };
    new.title = "A2".into();
    supersede_entry(&w.db, a, None, new, &agent())
        .await
        .unwrap();
    assert_eq!(
        reasons(&stale_of(&w, ca.id).await),
        vec![(entry(a), StaleKind::Superseded)]
    );
    retract_entry(&w.db, b, None, "неверно", &agent())
        .await
        .unwrap();
    assert_eq!(
        reasons(&stale_of(&w, cb.id).await),
        vec![(entry(b), StaleKind::Retracted)]
    );
    // тег без смены тела — действует; тело — changed
    let p = EntryPatch {
        tags: Some(vec!["x".into()]),
        ..EntryPatch::default()
    };
    update_entry(&w.db, c, Some(1), p, &agent()).await.unwrap();
    assert!(stale_of(&w, cc.id).await.is_empty());
    let p = EntryPatch {
        body: Some("другое тело".into()),
        ..EntryPatch::default()
    };
    update_entry(&w.db, c, Some(2), p, &agent()).await.unwrap();
    assert_eq!(
        reasons(&stale_of(&w, cc.id).await),
        vec![(entry(c), StaleKind::Changed)]
    );
    delete_entry(&w.db, d, Some(1), &agent()).await.unwrap();
    let got = cache_get(&w.db, cd.id).await.unwrap().unwrap();
    assert_eq!(got.entry.state, "stale");
    assert_eq!(reasons(&got.stale), vec![(entry(d), StaleKind::Deleted)]);
}

/// Новый источник: вектор чанка цели, изменённой после выжимки и не среди её источников, с
/// косинусом ≥ 0,80 к `query_vec` — `stale: new_sources`; источник самой выжимки, далёкий
/// вектор и другая область — нет.
#[tokio::test]
async fn new_sources_marks_stale() {
    let w = world("new-src").await;
    let n = note(&w, w.knowledge, "Старый", "# Старый\n").await;
    let mut p = put(&w, "bash", vec![src(node(n))]);
    p.query_vec = Some(vec![1.0, 0.0]);
    p.area = Some("web".into());
    let c = cache_put(&w.db, &p).await.unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    let fresh = note(&w, w.knowledge, "Новый", "# Новый\n").await;
    let far = note(&w, w.knowledge, "Далёкий", "# Далёкий\n").await;
    let k = w.knowledge;
    // свой источник (даже похожий) и далёкий вектор — не новые
    let vecs = vec![(node(n), vec![1.0, 0.0]), (node(far), vec![0.0, 1.0])];
    assert_eq!(
        mark_new_sources(&w.db, k, None, MODEL, &vecs, 0.80)
            .await
            .unwrap(),
        0
    );
    // другая область — мимо
    let vecs = vec![(node(fresh), vec![0.9, 0.1])];
    assert_eq!(
        mark_new_sources(&w.db, k, Some("api"), MODEL, &vecs, 0.80)
            .await
            .unwrap(),
        0
    );
    // другое пространство — мимо
    assert_eq!(
        mark_new_sources(&w.db, w.human, None, MODEL, &vecs, 0.80)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        mark_new_sources(&w.db, k, Some("web"), MODEL, &vecs, 0.80)
            .await
            .unwrap(),
        1
    );
    let got = cache_get(&w.db, c.id).await.unwrap().unwrap();
    assert_eq!(got.entry.state, "stale");
    assert_eq!(
        reasons(&got.stale),
        vec![(node(fresh), StaleKind::NewSources)]
    );
    // повтор не дублирует причину
    assert_eq!(
        mark_new_sources(&w.db, k, None, MODEL, &vecs, 0.80)
            .await
            .unwrap(),
        0
    );
}

/// Срок жизни: без обращений дольше срока и сверх предела (LRU) — удаляются; превращённые в
/// записи — нет. Превращение создаёт заметку и insight со связями `derived_from`.
#[tokio::test]
async fn ttl_and_lru_eviction_keeps_promoted() {
    let w = world("gc").await;
    let n = note(&w, w.knowledge, "Источник", "# И\n").await;
    let mut ids = Vec::new();
    for i in 0..4 {
        ids.push(
            cache_put(&w.db, &put(&w, &format!("q{i}"), vec![src(node(n))]))
                .await
                .unwrap()
                .id,
        );
    }
    // q0 — давно без обращений, q1 — превращена в заметку, q2 — в insight
    let old = Timestamp::now() - SignedDuration::from_hours(24 * 40);
    let (old_s, q0, q1) = (zam_store::time::fmt_ts(old), ids[0], ids[1]);
    w.db.write(move |t| {
        t.execute(
            "UPDATE answer_cache SET last_used_at = ?1 WHERE id IN (?2, ?3)",
            rusqlite::params![old_s, q0, q1],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let (_, note_ref) = cache_promote(
        &w.db,
        ids[1],
        PromoteAs::Note {
            path: NodePath::parse("Выжимки/q1").unwrap(),
        },
        &agent(),
    )
    .await
    .unwrap();
    let (e2, ins_ref) = cache_promote(
        &w.db,
        ids[2],
        PromoteAs::Insight {
            kind: "discovery".into(),
            title: "q2".into(),
        },
        &agent(),
    )
    .await
    .unwrap();
    assert_eq!(
        e2.promoted_to.as_deref(),
        Some(ins_ref.to_string().as_str())
    );
    assert!(matches!(
        cache_promote(
            &w.db,
            ids[2],
            PromoteAs::Insight {
                kind: "x".into(),
                title: "t".into()
            },
            &agent()
        )
        .await,
        Err(zam_store::StoreError::Conflict(_))
    ));
    let derived: i64 =
        w.db.read(move |c| {
            Ok(c.query_row(
                "SELECT count(*) FROM edges WHERE type = 'derived_from' AND dst_id = ?1
                 AND src_id IN (?2, ?3)",
                rusqlite::params![n, note_ref.id, ins_ref.id],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(derived, 2);

    let cutoff = Timestamp::now() - SignedDuration::from_hours(24 * 30);
    // предел 1: из живых непревращённых (q3) остаётся одна; q0 — по сроку
    let gc = gc_cache(&w.db, cutoff, 1).await.unwrap();
    assert_eq!((gc.expired, gc.evicted), (1, 0));
    assert!(cache_get(&w.db, ids[0]).await.unwrap().is_none());
    for id in &ids[1..] {
        assert!(cache_get(&w.db, *id).await.unwrap().is_some());
    }
    let extra = cache_put(&w.db, &put(&w, "q4", vec![src(node(n))]))
        .await
        .unwrap();
    let gc = gc_cache(&w.db, cutoff, 1).await.unwrap();
    assert_eq!((gc.expired, gc.evicted), (0, 1));
    assert!(cache_get(&w.db, ids[3]).await.unwrap().is_none());
    assert!(cache_get(&w.db, extra.id).await.unwrap().is_some());
    // превращённые (в том числе давняя q1) — на месте
    assert!(cache_get(&w.db, ids[1]).await.unwrap().is_some());
    assert!(cache_get(&w.db, ids[2]).await.unwrap().is_some());
}

/// Переход схемы v13 → v14 на базе с данными: копия `bak-v13`, таблицы кэша пусты и работают.
#[tokio::test]
async fn migrate_v13_to_v14() {
    const TS: &str = "2026-10-27T00:00:00.000000Z";
    let dd = datadir("mig-v14");
    let user = Id::new();
    let path = dd.user_db(&user);
    let space = Id::new();
    {
        let mut c = rusqlite::Connection::open(&path).unwrap();
        let v =
            zam_store::migrate::apply(&mut c, &path, &zam_store::USER_MIGRATIONS[..13]).unwrap();
        assert_eq!(v, 13);
        c.execute(
            "INSERT INTO sessions (id, started_at, last_seen_at) VALUES ('s', ?1, ?1)",
            [TS],
        )
        .unwrap();
        c.execute(
            "INSERT INTO spaces (id, kind, solution_guid, name, created_at)
             VALUES (?1, 'solution', 'g-m4-7', 'sol', ?2)",
            rusqlite::params![space, TS],
        )
        .unwrap();
    }
    let db = UserDbs::new(dd.users_dir()).get(user).await.unwrap();
    assert!(path.with_file_name(format!("{user}.db.bak-v13")).exists());
    let (ver, tables, sessions): (u32, i64, i64) = db
        .read(|c| {
            Ok((
                c.query_row("PRAGMA user_version", [], |r| r.get(0))?,
                c.query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type = 'table'
                     AND name IN ('answer_cache', 'cache_deps')",
                    [],
                    |r| r.get(0),
                )?,
                c.query_row("SELECT count(*) FROM sessions", [], |r| r.get(0))?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(ver, 14);
    assert_eq!((tables, sessions), (2, 1));
    assert_eq!(cache_count(&db).await.unwrap(), 0);
    let n = create_note(
        &db,
        NewNote {
            space_id: space,
            path: NodePath::parse("x").unwrap(),
            content: "# X\n".into(),
            l0: None,
            attrib: agent(),
        },
    )
    .await
    .unwrap();
    let mut p = CachePut {
        space_id: space,
        area: None,
        author_user_id: user,
        author_kind: AuthorKind::Agent,
        session_id: None,
        machine: None,
        scope_hash: scope(),
        query: "x".into(),
        query_vec: None,
        model_id: None,
        summary: "s".into(),
        tokens: 1,
        sources: vec![src(node(n.id))],
    };
    p.query = "после миграции".into();
    cache_put(&db, &p).await.unwrap();
    assert_eq!(cache_count(&db).await.unwrap(), 1);
}

/// Fix round 1 (I1): прежнюю выжимку того же ключа заменяет только тот же автор в той же
/// сессии; другая сессия и другой автор (участник решения) её не стирают.
#[tokio::test]
async fn put_replaces_only_same_author_and_session() {
    let w = world("replace").await;
    let n = note(&w, w.knowledge, "Bash", "# Bash\nLF.\n").await;
    let a = cache_put(&w.db, &put(&w, "bash", vec![src(node(n))]))
        .await
        .unwrap();
    // другая сессия того же автора
    let mut p = put(&w, "bash", vec![src(node(n))]);
    p.session_id = Some("s2".into());
    let b = cache_put(&w.db, &p).await.unwrap();
    // другой автор (участник) в той же сессии
    let mut p = put(&w, "bash", vec![src(node(n))]);
    p.author_user_id = Id::new();
    let c = cache_put(&w.db, &p).await.unwrap();
    // человек (WebUI) тем же пользователем
    let mut p = put(&w, "bash", vec![src(node(n))]);
    p.author_kind = AuthorKind::Human;
    let d = cache_put(&w.db, &p).await.unwrap();
    for id in [a.id, b.id, c.id, d.id] {
        assert!(cache_get(&w.db, id).await.unwrap().is_some());
    }
    // тот же автор и та же сессия — замена
    let a2 = cache_put(&w.db, &put(&w, "bash", vec![src(node(n))]))
        .await
        .unwrap();
    assert!(cache_get(&w.db, a.id).await.unwrap().is_none());
    assert_eq!(cache_count(&w.db).await.unwrap(), 4);
    // поиск: своя сессия первой, затем остальные — свежие первыми
    let mut q = query(&w, "bash");
    q.session_id = Some("s2".into());
    let ids: Vec<Id> = cache_lookup(&w.db, &q)
        .await
        .unwrap()
        .iter()
        .map(|h| h.entry.id)
        .collect();
    assert_eq!(ids, vec![b.id, a2.id]);
    // без чужих сессий — только своя
    q.cross_session = false;
    let hits = cache_lookup(&w.db, &q).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].entry.id, b.id);
    assert!(d.id != a2.id && c.id != a2.id);
}

/// Fix round 1 (I2): заметка и insight, в которые превращена выжимка, — не новые источники
/// (ни для неё самой, ни для других выжимок).
#[tokio::test]
async fn promoted_target_is_not_new_source() {
    let w = world("promoted-src").await;
    let n = note(&w, w.knowledge, "Источник", "# И\n").await;
    let mut p = put(&w, "bash", vec![src(node(n))]);
    p.query_vec = Some(vec![1.0, 0.0]);
    let c = cache_put(&w.db, &p).await.unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    let (_, made) = cache_promote(
        &w.db,
        c.id,
        PromoteAs::Note {
            path: NodePath::parse("Выжимки/bash").unwrap(),
        },
        &agent(),
    )
    .await
    .unwrap();
    let vecs = vec![(made, vec![1.0, 0.0])];
    assert_eq!(
        mark_new_sources(&w.db, w.knowledge, None, MODEL, &vecs, 0.80)
            .await
            .unwrap(),
        0
    );
    assert!(cache_get(&w.db, c.id).await.unwrap().unwrap().is_valid());
}

/// Fix round 1 (M4): замещённый или отозванный insight источником быть не может.
#[tokio::test]
async fn inactive_insight_source_rejected() {
    let w = world("inactive").await;
    let i = insight(&w, "Отозванный").await;
    retract_entry(&w.db, i, None, "неверно", &agent())
        .await
        .unwrap();
    let err = cache_put(&w.db, &put(&w, "q", vec![src(entry(i))]))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, CacheError::SourceInactive { status, .. } if status == "retracted"),
        "{err:?}"
    );
    assert_eq!(cache_count(&w.db).await.unwrap(), 0);
}

/// Fix round 1 (M6): вектор другой модели не сравнивается — ни семантическое попадание, ни
/// новые источники; точное совпадение работает.
#[tokio::test]
async fn other_model_vectors_ignored() {
    let w = world("model").await;
    let n = note(&w, w.knowledge, "Bash", "# Bash\n").await;
    let mut p = put(&w, "bash скрипты", vec![src(node(n))]);
    p.query_vec = Some(vec![1.0, 0.0]);
    p.model_id = Some("old/model@1".into());
    let c = cache_put(&w.db, &p).await.unwrap();
    let mut q = query(&w, "как писать bash");
    q.query_vec = Some(vec![1.0, 0.0]);
    assert!(cache_lookup(&w.db, &q).await.unwrap().is_empty());
    q.model_id = Some("old/model@1".into());
    assert_eq!(cache_lookup(&w.db, &q).await.unwrap().len(), 1);
    // точное — при любой модели
    let mut q = query(&w, "bash скрипты");
    q.model_id = None;
    assert_eq!(cache_lookup(&w.db, &q).await.unwrap().len(), 1);
    std::thread::sleep(std::time::Duration::from_millis(5));
    let fresh = note(&w, w.knowledge, "Новый", "# Новый\n").await;
    let vecs = vec![(node(fresh), vec![1.0, 0.0])];
    assert_eq!(
        mark_new_sources(&w.db, w.knowledge, None, MODEL, &vecs, 0.80)
            .await
            .unwrap(),
        0
    );
    assert!(cache_get(&w.db, c.id).await.unwrap().unwrap().is_valid());
}

/// Fix round 1 (M3): сохранение с превращением — одна транзакция: путь занят — `Conflict`,
/// выжимки нет.
#[tokio::test]
async fn put_with_promote_is_atomic() {
    let w = world("atomic").await;
    let n = note(&w, w.knowledge, "Занято", "# З\n").await;
    let to = PromoteAs::Note {
        path: NodePath::parse("Занято").unwrap(),
    };
    let err = cache_put_with(
        &w.db,
        &put(&w, "q", vec![src(node(n))]),
        Some((to, agent())),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, CacheError::Store(zam_store::StoreError::Conflict(_))),
        "{err:?}"
    );
    assert_eq!(cache_count(&w.db).await.unwrap(), 0);
    let to = PromoteAs::Note {
        path: NodePath::parse("Выжимки/q").unwrap(),
    };
    let (e, made) = cache_put_with(
        &w.db,
        &put(&w, "q", vec![src(node(n))]),
        Some((to, agent())),
    )
    .await
    .unwrap();
    assert_eq!(e.promoted_to, made.map(|t| t.to_string()));
}
