//! Поиск по точкам папок (задача 15 M3, §8.3, §9.2): канал иерархического вспоминания —
//! найденная папка поднимает листы своих потомков (`search.w_folder`), и карточки папок по
//! `filters.kinds ∋ folder` (не больше `search.max_folders`). Точки папок проверяются по
//! SQLite: удалённая папка, точка которой осталась в Qdrant, и Human не действуют; участник
//! общего решения видит только папки пространства решения.
//!
//! MiniLM на CPU-сборке ORT и настоящий Qdrant; без них тесты пропускаются (в CI — падают).

mod common;

use std::time::Duration;

use axum::http::{Method, StatusCode};
use common::api::{Api, api, vector_base};
use common::ml::{MINILM, external, have, ml, until, vector_ready};
use common::qdrant::TestQdrant;
use serde_json::{Value, json};
use zam_core::Id;
use zam_index::VectorFilter;
use zam_server::config::ServerConfig;

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e15";

/// Заметки папки `astronomy`: короткие, по одному листу; слово `collimation` — в одной.
const ASTRO: [(&str, &str); 3] = [
    (
        "astronomy/mirror",
        "# Telescope mirror\n\nGrind the parabolic glass mirror of a reflecting telescope \
         and check its collimation once before observing.\n",
    ),
    (
        "astronomy/eyepieces",
        "# Eyepieces\n\nPick eyepieces by focal length to change the magnification of \
         planets and nebulae.\n",
    ),
    (
        "astronomy/star-charts",
        "# Star charts\n\nUse a planisphere to find constellations and galaxies in the \
         night sky.\n",
    ),
];

/// Заметка в корне (не под папкой): слово `collimation` — в каждой фразе.
const ROOT: (&str, &str) = (
    "collimation",
    "# Collimation\n\nCollimation aligns optics. Laser collimation, collimation screws and \
     collimation caps: collimation keeps every lens of the instrument on one axis.\n",
);

/// Заметки других папок.
const OTHER: [(&str, &str); 3] = [
    (
        "kitchen/sourdough",
        "# Sourdough bread\n\nMix flour, water and a starter culture, knead the dough and \
         bake it in a very hot oven.\n",
    ),
    (
        "kitchen/coffee",
        "# Coffee brewing\n\nGrind beans right before brewing and steep for four minutes in \
         a French press.\n",
    ),
    (
        "garden/tomatoes",
        "# Tomato plants\n\nPlant seedlings in full sun, water the soil deeply and pick the \
         ripe tomatoes in late summer.\n",
    ),
];

fn sid(r: &common::api::Resp) -> String {
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["id"].as_str().unwrap().to_string()
}

fn items(j: &Value) -> &Vec<Value> {
    j["items"].as_array().unwrap_or_else(|| panic!("{j}"))
}

fn has_source(it: &Value, s: &str) -> bool {
    it["sources"]
        .as_array()
        .is_some_and(|a| a.iter().any(|x| x == s))
}

fn folder_cards(j: &Value) -> Vec<&Value> {
    items(j).iter().filter(|i| i["kind"] == "folder").collect()
}

/// Позиция первого элемента с путём под `prefix` (или равным ему).
fn pos_under(j: &Value, prefix: &str) -> Option<usize> {
    items(j).iter().position(|i| {
        i["kind"] != "folder"
            && i["path"]
                .as_str()
                .is_some_and(|p| p == prefix || p.starts_with(&format!("{prefix}/")))
    })
}

fn pos_path(j: &Value, path: &str) -> Option<usize> {
    items(j)
        .iter()
        .position(|i| i["kind"] != "folder" && i["path"] == path)
}

/// Точек папок в базе `user`.
async fn folder_points(a: &Api, user: Id) -> u64 {
    let f = VectorFilter {
        levels: vec!["folder".into()],
        ..vector_base(a, user).await
    };
    a.st.index.vector.as_ref().unwrap().count(&f).await.unwrap()
}

/// Состояние индексации узла базы `user`.
async fn state(a: &Api, user: Id, node: &str) -> String {
    let db = a.st.users.get(user).await.unwrap();
    zam_store::get_node(&db, node.parse().unwrap())
        .await
        .unwrap()
        .index_state
}

/// Ждёт индексации узлов `nodes` базы `user` и `folders` точек папок.
async fn wait_indexed(a: &Api, user: Id, nodes: &[String], folders: u64) {
    until(
        "заметки и папки проиндексированы",
        Duration::from_secs(240),
        async || {
            for n in nodes {
                if state(a, user, n).await != "indexed" {
                    return false;
                }
            }
            folder_points(a, user).await == folders
        },
    )
    .await;
}

/// Сервер с MiniLM и внешним Qdrant `q`; `tweak` — поверх.
async fn server(name: &str, q: &TestQdrant, tweak: impl FnOnce(&mut ServerConfig)) -> Api {
    let a = api(name, |c| {
        ml(c, MINILM);
        external(c, q);
        tweak(c);
    })
    .await;
    vector_ready(&a).await;
    a
}

/// Корпус в Knowledge администратора: `astronomy` (3), `kitchen` (2), `garden` (1) и
/// заметка в корне. Возвращает id заметок по пути; ждёт трёх точек папок.
async fn corpus(a: &Api) -> Vec<(String, String)> {
    let mut ids = Vec::new();
    for (p, b) in ASTRO.iter().chain(&[ROOT]).chain(&OTHER) {
        let n = a.note("knowledge", p, b).await;
        ids.push((p.to_string(), n["id"].as_str().unwrap().to_string()));
    }
    let nodes: Vec<String> = ids.iter().map(|(_, i)| i.clone()).collect();
    wait_indexed(a, a.st.admin_id, &nodes, 3).await;
    ids
}

async fn search(a: &Api, body: Value) -> Value {
    let r = a.t(Method::POST, "/api/v1/search", Some(body)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn broad_query_lifts_notes_under_matching_folder() {
    if !have(MINILM) {
        return;
    }
    let q = TestQdrant::start("fold-lift");
    // один корпус на двух серверах: M2 (w_folder = 0, умолчание) и с каналом папок
    let m2 = server("fold-lift-m2", &q, |_| {}).await;
    let fo = server("fold-lift-on", &q, |c| c.search.w_folder = 1.0).await;
    assert_eq!(
        m2.st.cfg.search.w_folder, 0.0,
        "канал выключен по умолчанию"
    );
    corpus(&m2).await;
    corpus(&fo).await;

    // в M2 корневая заметка первая в обоих каналах, заметка astronomy — вторая
    let body = json!({"query": "collimation", "top_k": 10});
    let before = search(&m2, body.clone()).await;
    let after = search(&fo, body).await;
    assert_eq!(after["degraded"], false, "{after}");
    // M2: канала папок нет, карточек нет; заметка в корне (слово в каждой фразе) — выше
    // заметок astronomy
    for it in items(&before) {
        assert!(!has_source(it, "folder"), "{before}");
        assert_ne!(it["kind"], "folder", "{before}");
    }
    let (root_m2, astro_m2) = (
        pos_path(&before, "collimation").expect("корень найден"),
        pos_under(&before, "astronomy").expect("astronomy найдена"),
    );
    assert!(root_m2 < astro_m2, "контроль корпуса: {before}");
    // с каналом: листы под папками получают источник `folder`, заметка в корне — нет;
    // карточек папок без `filters.kinds ∋ folder` нет
    assert!(folder_cards(&after).is_empty(), "{after}");
    for it in items(&after) {
        let p = it["path"].as_str().unwrap();
        assert_eq!(has_source(it, "folder"), p.contains('/'), "{p}: {after}");
    }
    // широкий запрос: заметка папки astronomy поднимается над заметкой в корне
    let (root_on, astro_on) = (
        pos_path(&after, "collimation").expect("корень найден"),
        pos_under(&after, "astronomy").expect("astronomy найдена"),
    );
    assert!(astro_on < root_on, "{after}");
    assert!(astro_on <= astro_m2, "{before}\n{after}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn folder_cards_only_with_kind_folder_and_capped() {
    if !have(MINILM) {
        return;
    }
    let q = TestQdrant::start("fold-cards");
    let a = server("fold-cards", &q, |_| {}).await;
    corpus(&a).await;
    let query = "astronomy telescope eyepieces";
    // по умолчанию (виды без folder) — карточек нет
    let j = search(&a, json!({"query": query})).await;
    assert!(folder_cards(&j).is_empty(), "{j}");
    // явный folder рядом с note: не больше max_folders (2) карточек из трёх папок, заметки —
    // как были (w_folder = 0: источника folder нет)
    let j = search(
        &a,
        json!({"query": query, "filters": {"kinds": ["note", "folder"]}}),
    )
    .await;
    let cards = folder_cards(&j);
    assert_eq!(cards.len(), 2, "{j}");
    assert!(items(&j).iter().any(|i| i["kind"] == "note"), "{j}");
    for it in items(&j).iter().filter(|i| i["kind"] == "note") {
        assert!(!has_source(it, "folder"), "{j}");
    }
    let astro = cards
        .iter()
        .find(|c| c["path"] == "astronomy")
        .unwrap_or_else(|| panic!("карточка astronomy: {j}"));
    assert_eq!(astro["level"], "folder", "{astro}");
    assert_eq!(astro["child_count"], 3, "{astro}");
    assert_eq!(astro["sources"], json!(["folder"]), "{astro}");
    assert_eq!(astro["chunk_ids"], json!([]), "{astro}");
    let s = astro["score"].as_f64().unwrap();
    assert!(s > 0.0 && s <= 1.0, "{astro}");
    assert!(
        astro["snippet"]
            .as_str()
            .is_some_and(|l| l.contains("mirror")),
        "L0 папки — свёртка детей: {astro}"
    );
    // только папки — одни карточки; max_folders ограничивает и их
    let j = search(
        &a,
        json!({"query": query, "filters": {"kinds": ["folder"]}}),
    )
    .await;
    assert_eq!(items(&j).len(), 2, "{j}");
    assert_eq!(folder_cards(&j).len(), 2, "{j}");
    assert_eq!(items(&j)[0]["path"], "astronomy", "{j}");
    // notes: L0 вместо snippet
    let j = search(
        &a,
        json!({"query": query, "mode": "notes", "filters": {"kinds": ["folder"]}}),
    )
    .await;
    assert!(
        items(&j)[0]["l0"]
            .as_str()
            .is_some_and(|l| l.contains("mirror")),
        "{j}"
    );
    // md: `f:<id> <пространство>/<путь>/  [score] · <N> детей`
    let r = a
        .t(
            Method::POST,
            "/api/v1/search",
            Some(json!({"query": query, "format": "md", "filters": {"kinds": ["folder"]}})),
        )
        .await;
    let t = r.text();
    let id = astro["node_id"].as_str().unwrap();
    assert!(
        t.starts_with(&format!("1. f:{id} Knowledge/astronomy/  [")),
        "{t}"
    );
    assert!(t.contains("] · 3 детей\n"), "{t}");
    assert_eq!(t.matches(". f:").count(), 2, "{t}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deleted_or_human_folder_ignored() {
    if !have(MINILM) {
        return;
    }
    let q = TestQdrant::start("fold-del");
    let a = server("fold-del", &q, |c| c.search.w_folder = 1.0).await;
    let ids = corpus(&a).await;
    let admin = a.st.admin_id;
    // Human: заметки под папкой — точки папки нет, карточки и подъёма тоже
    let r = a
        .adm(
            Method::POST,
            "/api/v1/nodes",
            Some(
                json!({"space": "human", "path": "diary/telescope", "kind": "note",
                        "content": ASTRO[0].1}),
            ),
            None,
        )
        .await;
    sid(&r);
    let body = json!({"query": "astronomy telescope", "top_k": 20,
                      "scope": {"include_human": true},
                      "filters": {"kinds": ["note", "folder"]}});
    let j = a
        .adm(Method::POST, "/api/v1/search", Some(body.clone()), None)
        .await
        .json();
    assert!(
        folder_cards(&j).iter().any(|c| c["path"] == "astronomy"),
        "контроль: живая папка находится: {j}"
    );
    for it in items(&j) {
        if it["path"].as_str().is_some_and(|p| p.starts_with("diary")) {
            assert_ne!(it["kind"], "folder", "{j}");
            assert!(!has_source(it, "folder"), "{j}");
        }
    }
    // проверка по SQLite: папка Human не проходит, даже если её id и пространство переданы
    let db = a.st.users.get(admin).await.unwrap();
    let spaces = zam_store::list_spaces(&db).await.unwrap();
    let human = spaces
        .iter()
        .find(|s| s.kind == zam_store::SpaceKind::Human)
        .unwrap()
        .id;
    let diary =
        zam_store::get_node_by_path(&db, human, &zam_store::NodePath::parse("diary").unwrap())
            .await
            .expect("папка Human");
    let all: Vec<Id> = spaces.iter().map(|s| s.id).collect();
    assert!(
        zam_store::live_folders(&db, vec![diary.id], &all, None)
            .await
            .unwrap()
            .is_empty()
    );

    // папка удалена в SQLite, а точка осталась в Qdrant (неудавшееся удаление)
    let astro_id = folder_card_id(&j, "astronomy");
    let points = folder_points(&a, admin).await;
    let id = astro_id.clone();
    db.write(move |tx| {
        tx.execute(
            "UPDATE nodes SET deleted_at = '2026-10-22T00:00:00Z' WHERE id = ?1",
            [id],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
        folder_points(&a, admin).await,
        points,
        "точка папки осталась"
    );
    let j = a
        .adm(Method::POST, "/api/v1/search", Some(body), None)
        .await
        .json();
    assert!(
        !folder_cards(&j).iter().any(|c| c["node_id"] == astro_id),
        "{j}"
    );
    // заметки удалённой папки больше не поднимаются ею
    let astro_notes: Vec<&str> = ids
        .iter()
        .filter(|(p, _)| p.starts_with("astronomy/"))
        .map(|(_, i)| i.as_str())
        .collect();
    for it in items(&j) {
        if astro_notes.contains(&it["node_id"].as_str().unwrap_or("")) {
            assert!(!has_source(it, "folder"), "{j}");
        }
    }
}

fn folder_card_id(j: &Value, path: &str) -> String {
    folder_cards(j)
        .iter()
        .find(|c| c["path"] == path)
        .unwrap_or_else(|| panic!("карточка {path}: {j}"))["node_id"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn member_sees_only_solution_folders() {
    if !have(MINILM) {
        return;
    }
    let q = TestQdrant::start("fold-member");
    let a = server("fold-member", &q, |c| c.search.w_folder = 1.0).await;
    let (a_id, a_tok) = a.new_user("Анна").await;
    let (b_id, b_tok) = a.new_user("Борис").await;
    let req = |tok: &str, m: Method, uri: &str, body: Option<Value>| {
        let (a, tok, uri) = (&a, tok.to_string(), uri.to_string());
        async move { a.tok(&tok, m, &uri, body).await }
    };
    // A — владелец решения G, B — одобренный участник
    let reg = json!({"agent_solution_id": G, "name": "Общее"});
    let r = req(&a_tok, Method::POST, "/api/v1/solutions", Some(reg.clone())).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = req(&b_tok, Method::POST, "/api/v1/solutions", Some(reg)).await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    let uri = format!("/api/v1/solutions/{G}/access/{b_id}/approve");
    assert_eq!(
        req(&a_tok, Method::POST, &uri, None).await.status,
        StatusCode::OK
    );
    let note = |tok: &str, space: &str, path: &str, content: &str| {
        let body = json!({"space": space, "path": path, "kind": "note", "content": content});
        let fut = req(tok, Method::POST, "/api/v1/nodes", Some(body));
        async move { sid(&fut.await) }
    };
    // Knowledge A: astronomy (вне решения); решение G: observatory; Knowledge B: optics
    let mut a_nodes = Vec::new();
    for (p, b) in ASTRO {
        a_nodes.push(note(&a_tok, "knowledge", p, b).await);
    }
    let sol_nodes = vec![
        note(&a_tok, G, "observatory/dome", ASTRO[0].1).await,
        note(&a_tok, G, "observatory/schedule", ASTRO[1].1).await,
    ];
    let b_nodes = vec![note(&b_tok, "knowledge", "optics/lens", ROOT.1).await];
    let (a_uid, b_uid): (Id, Id) = (a_id.parse().unwrap(), b_id.parse().unwrap());
    let mut a_all = a_nodes.clone();
    a_all.extend(sol_nodes.clone());
    wait_indexed(&a, a_uid, &a_all, 2).await;
    wait_indexed(&a, b_uid, &b_nodes, 1).await;

    let search_b = |body: Value| {
        let fut = req(&b_tok, Method::POST, "/api/v1/search", Some(body));
        async move {
            let r = fut.await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text());
            r.json()
        }
    };
    // контроль: владельцу папка astronomy его Knowledge находится
    let r = req(
        &a_tok,
        Method::POST,
        "/api/v1/search",
        Some(
            json!({"query": "telescope mirror", "scope": {"solution": G},
                    "filters": {"kinds": ["note", "folder"]}}),
        ),
    )
    .await;
    let j = r.json();
    assert!(
        folder_cards(&j).iter().any(|c| c["path"] == "astronomy"),
        "{j}"
    );
    // участник: только папки пространства решения (и своей базы), никогда — Knowledge A
    for scope in [
        json!({"solution": G}),
        json!({"all_solutions": true}),
        json!({"spaces": ["knowledge", G]}),
        json!({"solution": G, "path_prefix": "astronomy"}),
    ] {
        for kinds in [json!(["folder"]), json!(["note", "folder"])] {
            let j = search_b(json!({"query": "telescope mirror", "scope": scope,
                                     "top_k": 20, "filters": {"kinds": kinds}}))
            .await;
            for it in items(&j) {
                let p = it["path"].as_str().unwrap_or("");
                assert!(!p.starts_with("astronomy"), "{scope}: {j}");
                assert!(!a_nodes.iter().any(|n| it["node_id"] == n.as_str()), "{j}");
            }
        }
    }
    let j = search_b(
        json!({"query": "telescope mirror", "scope": {"solution": G},
                             "top_k": 20, "filters": {"kinds": ["note", "folder"]}}),
    )
    .await;
    let card = folder_cards(&j)
        .into_iter()
        .find(|c| c["path"] == "observatory")
        .unwrap_or_else(|| panic!("папка решения: {j}"));
    assert_eq!(card["agent_solution_id"], G, "{card}");
    assert_eq!(card["child_count"], 2, "{card}");
    // листы решения поднимаются папкой решения
    for it in items(&j).iter().filter(|i| i["kind"] == "note") {
        if it["path"]
            .as_str()
            .is_some_and(|p| p.starts_with("observatory/"))
        {
            assert!(has_source(it, "folder"), "{j}");
        }
    }
    // отзыв: папки решения больше не видны
    let uri = format!("/api/v1/solutions/{G}/access/{b_id}/revoke");
    assert_eq!(
        req(&a_tok, Method::POST, &uri, None).await.status,
        StatusCode::OK
    );
    let j = search_b(
        json!({"query": "telescope mirror", "scope": {"solution": G},
                             "top_k": 20, "filters": {"kinds": ["note", "folder"]}}),
    )
    .await;
    assert!(
        !items(&j).iter().any(|i| i["path"]
            .as_str()
            .is_some_and(|p| p.starts_with("observatory"))),
        "{j}"
    );
    assert_eq!(j["warnings"], json!(["solution access revoked"]), "{j}");
}
