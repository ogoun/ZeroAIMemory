//! Записи журналов в векторном индексе и выдаче `POST /search` (задача 6 M3, §7.5, §9.1–9.3):
//! payload точек записи (`entry_id`, `kind`, `status`, `insight_kind`, `certainty`, `area`),
//! элементы и карточки записей, фильтр статусов по умолчанию и `include_history`, обновление
//! payload при смене статуса. Векторные тесты — MiniLM на CPU-сборке ORT и настоящий Qdrant;
//! без них пропускаются (в CI, `ZAM_REQUIRE_ML=1`, — падают). Изоляция участника решения —
//! без моделей (FTS).
mod common;

use std::time::Duration;

use axum::http::{Method, StatusCode};
use common::api::{Api, api, vector_base};
use common::ml::{MINILM, external, have, ml, until, vector_ready};
use common::qdrant::TestQdrant;
use serde_json::{Value, json};
use zam_core::Id;
use zam_index::VectorFilter;
use zam_store::{
    Attribution, AuthorKind, Certainty, Db, Entry, EntryKind, EntryPatch, EntryStatus, IndexState,
    NewEntry, SpaceKind, create_entry, get_entry, job_stats, list_spaces, retract_entry,
    supersede_entry, update_entry,
};

/// Предел ожидания индексации (инференс на CPU, запись в Qdrant на HDD).
const INDEXED: Duration = Duration::from_secs(240);

fn agent() -> Attribution {
    let mut a = Attribution::new(AuthorKind::Agent);
    a.area = Some("web".into());
    a
}

fn insight(space: Id, title: &str, body: &str) -> NewEntry {
    NewEntry {
        refs: Vec::new(),
        space_id: space,
        kind: EntryKind::Insight,
        title: title.into(),
        body: body.into(),
        data: json!({"kind": "gotcha"}),
        area: None,
        tags: vec!["bash".into()],
        status: None,
        certainty: Some(Certainty::Hypothesis),
        idempotency_key: None,
        triggers: Vec::new(),
        links: Vec::new(),
    }
}

/// Пространство вида `kind` базы `db`.
async fn space(db: &Db, kind: SpaceKind) -> Id {
    list_spaces(db)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.kind == kind)
        .unwrap()
        .id
}

/// Сервер с MiniLM и внешним Qdrant: `(Api, Qdrant, база администратора, Knowledge)`.
async fn setup(name: &str) -> Option<(Api, TestQdrant, Db, Id)> {
    if !have(MINILM) {
        return None;
    }
    let q = TestQdrant::start(name);
    let a = api(name, |c| {
        ml(c, MINILM);
        external(c, &q);
    })
    .await;
    vector_ready(&a).await;
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let k = space(&db, SpaceKind::Knowledge).await;
    // заметка рядом: выдача смешанная, фильтр видов отделяет записи от заметок
    a.note(
        "knowledge",
        "dev/editors",
        "# Text editors\n\nConfigure the editor to keep unix line endings for shell files.\n",
    )
    .await;
    Some((a, q, db, k))
}

/// Ждёт, пока записи `ids` станут `indexed` (воркер будится), а затем — покоя воркера:
/// очередь заданий базы пуста, пачка не выполняется, отложенных свёрток папок нет. Без покоя
/// счётчик `embedded_texts` мог «доехать» позже: заметка из [`setup`] и свёртка L0 её папки
/// (`dev`, отложенная на `rollup_debounce_ms`) индексируются параллельно с записями, и под
/// нагрузкой их эмбеддинг попадал между снятием базового счётчика и проверкой (флейк
/// `status_change_updates_payload`: 5 вместо 4).
async fn wait_indexed(a: &Api, db: &Db, ids: &[Id]) {
    a.st.index.worker.notify();
    until(
        "записи проиндексированы",
        INDEXED,
        async || {
            for id in ids {
                if get_entry(db, *id).await.unwrap().index_state != IndexState::Indexed {
                    return false;
                }
            }
            true
        },
    )
    .await;
    // покой: условие должно держаться при двух проверках подряд (свёртка папки ставится по
    // закрытию задания, пачка может начаться между проверками)
    let idle = async || {
        let st = job_stats(db).await.unwrap();
        let w = a.st.index.worker.status();
        st.queued + st.running == 0 && !w.batch_running && w.rollups_pending == 0
    };
    until("покой воркера", INDEXED, async || {
        if !idle().await {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        idle().await
    })
    .await;
}

/// Точек администратора по фильтру `f` поверх всей базы (все уровни чанков).
async fn points(a: &Api, f: impl FnOnce(&mut VectorFilter)) -> u64 {
    let mut filter = VectorFilter {
        levels: ["chunk", "section", "note"].map(String::from).to_vec(),
        ..vector_base(a, a.st.admin_id).await
    };
    f(&mut filter);
    a.st.index
        .vector
        .as_ref()
        .unwrap()
        .count(&filter)
        .await
        .unwrap()
}

async fn search(a: &Api, body: Value) -> Value {
    let r = a.t(Method::POST, "/api/v1/search", Some(body)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

/// `entry_id` элементов выдачи (элементы заметок пропускаются).
fn entry_ids(j: &Value) -> Vec<String> {
    j["items"]
        .as_array()
        .unwrap_or_else(|| panic!("{j}"))
        .iter()
        .filter_map(|i| i["entry_id"].as_str().map(str::to_string))
        .collect()
}

fn item<'a>(j: &'a Value, e: &Entry) -> &'a Value {
    j["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["entry_id"] == json!(e.id))
        .unwrap_or_else(|| panic!("нет записи {}: {j}", e.id))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn entry_indexed_with_payload() {
    let Some((a, _q, db, k)) = setup("entry-payload").await else {
        return;
    };
    let (e, _) = create_entry(
        &db,
        insight(
            k,
            "Shell scripts need LF line endings",
            "A script saved with CRLF fails with bad interpreter.",
        ),
        &agent(),
    )
    .await
    .unwrap();
    wait_indexed(&a, &db, &[e.id]).await;
    let all = points(&a, |f| f.kinds = vec!["insight".into()]).await;
    assert!(all > 0, "у записи нет точек");
    // поля записи в payload: вид инсайта, certainty, область, статус
    let same = [
        points(&a, |f| {
            f.kinds = vec!["insight".into()];
            f.insight_kinds = vec!["gotcha".into()];
        })
        .await,
        points(&a, |f| {
            f.kinds = vec!["insight".into()];
            f.certainty = vec!["hypothesis".into()];
        })
        .await,
        points(&a, |f| {
            f.kinds = vec!["insight".into()];
            f.area = Some("web".into());
        })
        .await,
        points(&a, |f| {
            f.kinds = vec!["insight".into()];
            f.exclude_statuses = vec!["superseded".into(), "retracted".into()];
        })
        .await,
    ];
    assert_eq!(same, [all; 4]);
    for none in [
        points(&a, |f| {
            f.kinds = vec!["insight".into()];
            f.certainty = vec!["axiom".into()];
        })
        .await,
        points(&a, |f| {
            f.kinds = vec!["insight".into()];
            f.insight_kinds = vec!["failure".into()];
        })
        .await,
        points(&a, |f| {
            f.kinds = vec!["insight".into()];
            f.exclude_statuses = vec!["active".into()];
        })
        .await,
        // у записи нет пути: префикс пути её не пропускает
        points(&a, |f| {
            f.kinds = vec!["insight".into()];
            f.path_prefix = Some("dev".into());
        })
        .await,
    ] {
        assert_eq!(none, 0);
    }
    // точки заметки — отдельно, вид `note`
    assert!(points(&a, |f| f.kinds = vec!["note".into()]).await > 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn search_returns_entry_items_with_kind_status() {
    let Some((a, _q, db, k)) = setup("entry-search").await else {
        return;
    };
    let (ins, _) = create_entry(
        &db,
        insight(
            k,
            "Shell scripts need LF line endings",
            "A script saved with CRLF fails with bad interpreter.",
        ),
        &agent(),
    )
    .await
    .unwrap();
    let (ch, _) = create_entry(
        &db,
        NewEntry {
            refs: Vec::new(),
            space_id: k,
            kind: EntryKind::Changelog,
            title: "Converted deploy scripts to LF".into(),
            body: "All shell scripts in the repository now use unix line endings.".into(),
            data: json!({"kind": "fix"}),
            area: None,
            tags: Vec::new(),
            status: None,
            certainty: None,
            idempotency_key: None,
            triggers: Vec::new(),
            links: Vec::new(),
        },
        &agent(),
    )
    .await
    .unwrap();
    wait_indexed(&a, &db, &[ins.id, ch.id]).await;

    let j = search(&a, json!({"query": "bad interpreter CRLF", "top_k": 10})).await;
    assert_eq!(j["degraded"], false, "{j}");
    let it = item(&j, &ins);
    assert!(it.get("node_id").is_none(), "{it}");
    assert_eq!(it["kind"], "insight", "{it}");
    assert_eq!(it["status"], "active", "{it}");
    assert_eq!(it["insight_kind"], "gotcha", "{it}");
    assert_eq!(it["certainty"], "hypothesis", "{it}");
    assert_eq!(it["title"], "Shell scripts need LF line endings", "{it}");
    assert_eq!(it["space"]["kind"], "knowledge", "{it}");

    // перефраз без общих слов с текстом записи — векторный канал находит её по смыслу
    let j = search(
        &a,
        json!({"query": "carriage return breaks shebang execution", "top_k": 10}),
    )
    .await;
    let found = entry_ids(&j);
    assert!(found.contains(&ins.id.to_string()), "{j}");
    let it = item(&j, &ins);
    let src = it["sources"].as_array().unwrap();
    assert!(src.contains(&json!("vector")), "{it}");

    // changelog — элемент без статуса
    let j = search(&a, json!({"query": "deploy scripts", "top_k": 10})).await;
    let it = item(&j, &ch);
    assert_eq!(it["kind"], "changelog", "{it}");
    assert!(it["status"].is_null(), "{it}");
    assert_eq!(it["insight_kind"], "fix", "{it}");

    // карточки md (§9.3): i: и c:
    let r = a
        .t(
            Method::POST,
            "/api/v1/search",
            Some(json!({"query": "line endings scripts", "format": "md", "top_k": 10})),
        )
        .await;
    let t = r.text();
    assert!(
        t.contains(&format!(
            "i:{} insight/gotcha «Shell scripts need LF line endings»  [",
            ins.id
        )),
        "{t}"
    );
    assert!(t.contains("] active · hypothesis · ~"), "{t}");
    assert!(
        t.contains(&format!(
            "c:{} changelog/fix «Converted deploy scripts to LF»  [",
            ch.id
        )),
        "{t}"
    );
    assert!(t.contains("n:"), "заметка тоже в выдаче: {t}");

    // filters.kinds: только заметки — записей нет; только insight — нет заметок и changelog
    let j = search(
        &a,
        json!({"query": "line endings scripts", "filters": {"kinds": ["note"]}}),
    )
    .await;
    assert!(entry_ids(&j).is_empty(), "{j}");
    assert!(!j["items"].as_array().unwrap().is_empty(), "{j}");
    let j = search(
        &a,
        json!({"query": "line endings scripts", "filters": {"kinds": ["insight"]}}),
    )
    .await;
    let items = j["items"].as_array().unwrap();
    assert!(!items.is_empty(), "{j}");
    assert!(items.iter().all(|i| i["kind"] == "insight"), "{j}");
    // неизвестный вид — 400
    let r = a
        .t(
            Method::POST,
            "/api/v1/search",
            Some(json!({"query": "x", "filters": {"kinds": ["nope"]}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn default_excludes_superseded_and_retracted_include_history_returns() {
    let Some((a, _q, db, k)) = setup("entry-history").await else {
        return;
    };
    let (old, _) = create_entry(
        &db,
        insight(
            k,
            "Zebrafox cache must be warmed manually",
            "Warm zebrafox before use.",
        ),
        &agent(),
    )
    .await
    .unwrap();
    let (new, _) = supersede_entry(
        &db,
        old.id,
        None,
        insight(
            k,
            "Zebrafox cache warms itself",
            "Zebrafox warms on first request.",
        ),
        &agent(),
    )
    .await
    .unwrap();
    let (gone, _) = create_entry(
        &db,
        insight(
            k,
            "Zebrafox needs a restart daily",
            "Restart zebrafox every day.",
        ),
        &agent(),
    )
    .await
    .unwrap();
    let gone = retract_entry(&db, gone.id, None, "refuted by monitoring", &agent())
        .await
        .unwrap();
    wait_indexed(&a, &db, &[old.id, new.id, gone.id]).await;

    // по умолчанию — только действующая; и в FTS, и в векторном канале
    let j = search(&a, json!({"query": "zebrafox cache", "top_k": 20})).await;
    assert_eq!(j["degraded"], false, "{j}");
    assert_eq!(entry_ids(&j), vec![new.id.to_string()], "{j}");
    let it = item(&j, &new);
    let src = it["sources"].as_array().unwrap();
    assert!(
        src.contains(&json!("vector")) && src.contains(&json!("fts")),
        "{it}"
    );
    // векторный фильтр сам отсекает историю: точки старой и отозванной есть, но не проходят
    let live = points(&a, |f| {
        f.kinds = vec!["insight".into()];
        f.exclude_statuses = vec!["superseded".into(), "retracted".into()];
    })
    .await;
    let all = points(&a, |f| f.kinds = vec!["insight".into()]).await;
    assert!(live > 0 && all > live, "{live} / {all}");

    // include_history — все три со своими статусами
    let j = search(
        &a,
        json!({"query": "zebrafox", "top_k": 20, "include_history": true}),
    )
    .await;
    let mut got = entry_ids(&j);
    got.sort();
    let mut want = vec![old.id.to_string(), new.id.to_string(), gone.id.to_string()];
    want.sort();
    assert_eq!(got, want, "{j}");
    assert_eq!(item(&j, &old)["status"], "superseded", "{j}");
    assert_eq!(item(&j, &gone)["status"], "retracted", "{j}");
    // явный filters.status
    let j = search(
        &a,
        json!({"query": "zebrafox", "top_k": 20, "filters": {"status": ["retracted"]}}),
    )
    .await;
    assert_eq!(entry_ids(&j), vec![gone.id.to_string()], "{j}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn status_change_updates_payload() {
    let Some((a, _q, db, k)) = setup("entry-status").await else {
        return;
    };
    let (e, _) = create_entry(
        &db,
        insight(
            k,
            "Quokkasaur index needs vacuum",
            "Run vacuum on quokkasaur weekly.",
        ),
        &agent(),
    )
    .await
    .unwrap();
    wait_indexed(&a, &db, &[e.id]).await;
    let all = points(&a, |f| f.kinds = vec!["insight".into()]).await;
    assert!(all > 0);
    let with = |status: &'static str, certainty: &'static str| {
        let a = &a;
        async move {
            points(a, |f| {
                f.kinds = vec!["insight".into()];
                f.exclude_statuses = super_statuses_except(status);
                f.certainty = vec![certainty.into()];
            })
            .await
        }
    };
    assert_eq!(with("active", "hypothesis").await, all);

    // смена статуса и certainty — upsert с новым payload, тексты те же (эмбеддинги из кэша)
    let embedded = a.st.index.worker.embedded_texts();
    let p = EntryPatch {
        status: Some(EntryStatus::Revisit),
        certainty: Some(Some(Certainty::Theorem)),
        reason: Some("checked on staging".into()),
        ..EntryPatch::default()
    };
    update_entry(&db, e.id, None, p, &Attribution::new(AuthorKind::Human))
        .await
        .unwrap();
    wait_indexed(&a, &db, &[e.id]).await;
    assert_eq!(with("revisit", "theorem").await, all);
    assert_eq!(with("active", "hypothesis").await, 0);
    assert_eq!(
        a.st.index.worker.embedded_texts(),
        embedded,
        "смена статуса не требует инференса"
    );
    let j = search(&a, json!({"query": "quokkasaur vacuum"})).await;
    let it = item(&j, &e);
    assert_eq!(
        (&it["status"], &it["certainty"]),
        (&json!("revisit"), &json!("theorem"))
    );

    // отзыв — запись уходит из выдачи по умолчанию, точки остаются со статусом retracted
    retract_entry(&db, e.id, None, "vacuum is automatic now", &agent())
        .await
        .unwrap();
    wait_indexed(&a, &db, &[e.id]).await;
    assert_eq!(with("retracted", "theorem").await, all);
    let j = search(&a, json!({"query": "quokkasaur vacuum"})).await;
    assert!(entry_ids(&j).is_empty(), "{j}");
    let j = search(
        &a,
        json!({"query": "quokkasaur vacuum", "include_history": true}),
    )
    .await;
    assert_eq!(item(&j, &e)["status"], "retracted", "{j}");
}

/// Все статусы, кроме `keep`: фильтр «только `keep`» через `must_not`.
fn super_statuses_except(keep: &str) -> Vec<String> {
    ["active", "revisit", "superseded", "retracted"]
        .into_iter()
        .filter(|s| *s != keep)
        .map(String::from)
        .collect()
}

/// Участник общего решения находит записи журнала решения в базе владельца и не видит
/// записей владельца вне решения (Knowledge) — фильтр пространств цели действует и для
/// записей (Review Focus 2). Без моделей: только FTS.
#[tokio::test]
async fn member_sees_only_solution_entries() {
    const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e77";
    const WORD: &str = "квокказавр";
    let a = api("entry-member", |_| {}).await;
    let (a_id, a_tok) = a.new_user("Анна").await;
    let (b_id, b_tok) = a.new_user("Борис").await;
    let body = json!({ "agent_solution_id": G, "name": "Общее" });
    let r = a
        .tok(
            &a_tok,
            Method::POST,
            "/api/v1/solutions",
            Some(body.clone()),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = a
        .tok(&b_tok, Method::POST, "/api/v1/solutions", Some(body))
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    let r = a
        .tok(
            &a_tok,
            Method::POST,
            &format!("/api/v1/solutions/{G}/access/{b_id}/approve"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    let owner: Id = a_id.parse().unwrap();
    let db = a.st.users.get(owner).await.unwrap();
    let sol = list_spaces(&db)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.solution_guid.as_deref() == Some(G))
        .unwrap()
        .id;
    let k = space(&db, SpaceKind::Knowledge).await;
    let (in_sol, _) = create_entry(
        &db,
        insight(sol, &format!("{WORD} в решении"), "b"),
        &agent(),
    )
    .await
    .unwrap();
    let (private, _) = create_entry(&db, insight(k, &format!("{WORD} владельца"), "b"), &agent())
        .await
        .unwrap();
    let search = |tok: String| {
        let a = &a;
        async move {
            let r = a
                .tok(
                    &tok,
                    Method::POST,
                    "/api/v1/search",
                    Some(json!({"query": WORD, "scope": {"solution": G}})),
                )
                .await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text());
            r.json()
        }
    };
    let j = search(b_tok.clone()).await;
    assert_eq!(entry_ids(&j), vec![in_sol.id.to_string()], "{j}");
    assert_eq!(item(&j, &in_sol)["agent_solution_id"], G, "{j}");
    // владелец видит обе
    let mut got = entry_ids(&search(a_tok.clone()).await);
    got.sort();
    let mut want = vec![in_sol.id.to_string(), private.id.to_string()];
    want.sort();
    assert_eq!(got, want);
}
