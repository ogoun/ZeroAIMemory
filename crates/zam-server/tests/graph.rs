//! Граф связей через REST (задача 10 M3, §8.5, §12): `GET /graph`, `POST /edges`,
//! `DELETE /edges/{id}`, обратные и исходящие ссылки, `links` insight, `expand=graph` поиска,
//! согласие рендера и рёбер, изоляция участника общего решения и Human.
//!
//! Серверы без моделей (FTS). Пользователи: A — владелец решения, B — участник.
mod common;

use axum::http::{Method, StatusCode};
use common::api::{Api, Resp, api};
use serde_json::{Value, json};

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e31";

fn id(v: &Value) -> String {
    v["id"].as_str().unwrap().to_string()
}

async fn get(a: &Api, tok: &str, uri: &str) -> Resp {
    a.tok(tok, Method::GET, uri, None).await
}

async fn edge(a: &Api, tok: &str, src: &str, kind: &str, dst: &str) -> Resp {
    a.tok(
        tok,
        Method::POST,
        "/api/v1/edges",
        Some(json!({"src": src, "type": kind, "dst": dst})),
    )
    .await
}

async fn note_by(a: &Api, tok: &str, space: &str, path: &str, content: &str) -> Value {
    let r = a
        .tok(
            tok,
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": space, "path": path, "kind": "note", "content": content})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()
}

fn insight(title: &str, body: &str) -> Value {
    json!({"kind": "insight", "title": title, "body": body, "data": {"kind": "gotcha"}, "force": true})
}

async fn create_insight(a: &Api, tok: &str, space: &str, body: Value) -> Value {
    let r = a
        .tok(
            tok,
            Method::POST,
            &format!("/api/v1/spaces/{space}/entries"),
            Some(body),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()
}

/// `ref` узлов графа.
fn graph_refs(j: &Value) -> Vec<String> {
    let mut v: Vec<String> = j["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["ref"].as_str().unwrap().to_string())
        .collect();
    v.sort();
    v
}

fn srcs(j: &Value) -> Vec<String> {
    j["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["src"].as_str().unwrap().to_string())
        .collect()
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[tokio::test]
async fn graph_api_and_backlinks() {
    let a = api("graph-api", |_| {}).await;
    let tok = a.token.clone();
    let b = a.note("knowledge", "b", "# B\n").await;
    let n_a = a.note("knowledge", "a", "# A\n\n[[b]] и [[Нет]]\n").await;
    // неизвестный тип relations — предупреждение в ответе записи, ребра нет
    let r = a
        .t(
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": "knowledge", "path": "c", "kind": "note",
                        "content": "---\nrelations: [\"depends_on: [[a]]\", \"blocks: [[b]]\"]\n---\n# C\n"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let c = r.json();
    assert_eq!(c["warnings"], json!(["unknown relation type: blocks"]));
    let (b_id, a_id, c_id) = (id(&b), id(&n_a), id(&c));

    let r = get(&a, &tok, &format!("/api/v1/graph?node={a_id}&depth=1")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(
        graph_refs(&j),
        sorted(vec![
            format!("n:{a_id}"),
            format!("n:{b_id}"),
            format!("n:{c_id}")
        ])
    );
    assert_eq!(j["edges"].as_array().unwrap().len(), 2, "{j}");
    let only = get(
        &a,
        &tok,
        &format!("/api/v1/graph?node={a_id}&depth=2&types=depends_on"),
    )
    .await
    .json();
    assert_eq!(
        graph_refs(&only),
        sorted(vec![format!("n:{a_id}"), format!("n:{c_id}")])
    );
    assert_eq!(only["edges"][0]["type"], "depends_on");
    assert_eq!(only["edges"][0]["origin"], "frontmatter");
    for bad in [
        format!("/api/v1/graph?node={a_id}&depth=4"),
        format!("/api/v1/graph?node={a_id}&depth=0"),
        format!("/api/v1/graph?node={a_id}&types=blocks"),
        format!("/api/v1/graph?node={a_id}&entry={a_id}"),
        "/api/v1/graph".to_string(),
    ] {
        assert_eq!(
            get(&a, &tok, &bad).await.status,
            StatusCode::BAD_REQUEST,
            "{bad}"
        );
    }

    let r = get(&a, &tok, &format!("/api/v1/nodes/{b_id}/backlinks")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(srcs(&j), vec![format!("n:{a_id}")]);
    assert_eq!(j["items"][0]["type"], "links_to");
    assert_eq!(j["items"][0]["dst_raw"], "[[b]]");
    // узлы другого конца — с заголовком и путём
    assert_eq!(j["nodes"][0]["ref"], format!("n:{a_id}"));
    assert_eq!(j["nodes"][0]["path"], "a");
    let r = get(&a, &tok, &format!("/api/v1/nodes/{a_id}/links")).await;
    let j = r.json();
    let raws: Vec<&str> = j["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["dst_raw"].as_str().unwrap())
        .collect();
    assert_eq!(raws, vec!["[[b]]", "[[Нет]]"]);
    assert_eq!(j["items"][1]["dst"], Value::Null);

    // запись журнала: явная связь заметка → insight, обратные ссылки записи
    let ins = create_insight(&a, &tok, "knowledge", insight("вывод про сборку", "тело")).await;
    let ins_id = id(&ins);
    let r = edge(
        &a,
        &tok,
        &format!("n:{b_id}"),
        "relates_to",
        &format!("entry:{ins_id}"),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let j = get(&a, &tok, &format!("/api/v1/entries/{ins_id}/backlinks"))
        .await
        .json();
    assert_eq!(srcs(&j), vec![format!("n:{b_id}")]);
    // исходящие связи записи: явное ребро insight → заметка
    let r = edge(
        &a,
        &tok,
        &format!("e:{ins_id}"),
        "relates_to",
        &format!("node:{a_id}"),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = get(&a, &tok, &format!("/api/v1/entries/{ins_id}/links")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["items"].as_array().unwrap().len(), 1, "{j}");
    assert_eq!(j["items"][0]["src"], format!("e:{ins_id}"));
    assert_eq!(j["items"][0]["dst"], format!("n:{a_id}"));
    assert_eq!(j["nodes"][0]["path"], "a");
    assert_eq!(
        get(
            &a,
            &tok,
            &format!("/api/v1/entries/{}/links", zam_core::Id::new())
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    let r = get(&a, &tok, &format!("/api/v1/graph?entry={ins_id}")).await;
    assert_eq!(
        graph_refs(&r.json()),
        sorted(vec![
            format!("e:{ins_id}"),
            format!("n:{a_id}"),
            format!("n:{b_id}")
        ])
    );

    // Human не виден токену ни узлом, ни соседом, ни обратной ссылкой
    let r = a
        .adm(
            Method::POST,
            "/api/v1/nodes",
            Some(
                json!({"space": "human", "path": "дневник", "kind": "note", "content": "личное\n"}),
            ),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let h_id = id(&r.json());
    let r = a
        .adm(
            Method::POST,
            "/api/v1/edges",
            Some(
                json!({"src": format!("n:{h_id}"), "type": "mentions", "dst": format!("n:{b_id}")}),
            ),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let j = get(&a, &tok, &format!("/api/v1/graph?node={b_id}"))
        .await
        .json();
    assert!(!graph_refs(&j).contains(&format!("n:{h_id}")), "{j}");
    let j = get(&a, &tok, &format!("/api/v1/nodes/{b_id}/backlinks"))
        .await
        .json();
    assert!(!srcs(&j).contains(&format!("n:{h_id}")), "{j}");
    // администратор видит
    let j = a
        .adm(
            Method::GET,
            &format!("/api/v1/nodes/{b_id}/backlinks"),
            None,
            None,
        )
        .await
        .json();
    assert!(srcs(&j).contains(&format!("n:{h_id}")), "{j}");
    for uri in [
        format!("/api/v1/graph?node={h_id}"),
        format!("/api/v1/nodes/{h_id}/backlinks"),
        format!("/api/v1/nodes/{h_id}/links"),
    ] {
        assert_eq!(
            get(&a, &tok, &uri).await.status,
            StatusCode::NOT_FOUND,
            "{uri}"
        );
    }
    let r = edge(
        &a,
        &tok,
        &format!("n:{b_id}"),
        "relates_to",
        &format!("n:{h_id}"),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());

    // удалённый узел — 404
    let r = a
        .t(Method::DELETE, &format!("/api/v1/nodes/{c_id}"), None)
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(
        get(&a, &tok, &format!("/api/v1/graph?node={c_id}"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn explicit_edge_crud_system_protected() {
    let a = api("graph-crud", |_| {}).await;
    let tok = a.token.clone();
    let x = id(&a.note("knowledge", "x", "# X\n").await);
    let y = id(&a.note("knowledge", "папка/Игрек", "# Y\n").await);
    let src = format!("n:{x}");
    let r = edge(&a, &tok, &src, "depends_on", "[[Игрек]]").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let e = r.json();
    assert_eq!(
        (
            e["src"].as_str(),
            e["dst"].as_str(),
            e["type"].as_str(),
            e["origin"].as_str()
        ),
        (
            Some(src.as_str()),
            Some(format!("n:{y}").as_str()),
            Some("depends_on"),
            Some("explicit")
        )
    );
    // повтор — 409; формы ссылок node:/n: равны
    let r = edge(
        &a,
        &tok,
        &format!("node:{x}"),
        "depends_on",
        &format!("n:{y}"),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    for (kind, dst, want) in [
        (
            "supersedes",
            format!("n:{y}"),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        ("blocks", format!("n:{y}"), StatusCode::UNPROCESSABLE_ENTITY),
        (
            "relates_to",
            "[[Нет такой]]".to_string(),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        ("relates_to", "мусор".to_string(), StatusCode::BAD_REQUEST),
        (
            "relates_to",
            format!("n:{}", zam_core::Id::new()),
            StatusCode::NOT_FOUND,
        ),
        ("relates_to", src.clone(), StatusCode::BAD_REQUEST),
    ] {
        let r = edge(&a, &tok, &src, kind, &dst).await;
        assert_eq!(r.status, want, "{kind} {dst}: {}", r.text());
    }
    // ro-токен не создаёт и не удаляет
    let admin = a.admin_id();
    let ro = a.issue(&admin, "ro").await;
    let r = edge(&a, &ro, &src, "mentions", &format!("n:{y}")).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let eid = id(&e);
    let r = a
        .tok(&ro, Method::DELETE, &format!("/api/v1/edges/{eid}"), None)
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let r = a
        .t(Method::DELETE, &format!("/api/v1/edges/{eid}"), None)
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    let r = a
        .t(Method::DELETE, &format!("/api/v1/edges/{eid}"), None)
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    // выведенное и системное рёбра вручную не удаляются — 422
    let z = id(&a.note("knowledge", "z", "[[x]]\n").await);
    let links = get(&a, &tok, &format!("/api/v1/nodes/{z}/links"))
        .await
        .json();
    let derived = id(&links["items"][0]);
    let r = a
        .t(Method::DELETE, &format!("/api/v1/edges/{derived}"), None)
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    let old = create_insight(&a, &tok, "knowledge", insight("старый вывод", "раз")).await;
    let old_id = id(&old);
    let r = a
        .t(
            Method::POST,
            &format!("/api/v1/entries/{old_id}/supersede"),
            Some(json!({"title": "новый вывод", "body": "два", "data": {"kind": "gotcha"}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let new_id = id(&r.json()["entry"]);
    let back = get(&a, &tok, &format!("/api/v1/entries/{old_id}/backlinks"))
        .await
        .json();
    assert_eq!(srcs(&back), vec![format!("e:{new_id}")]);
    assert_eq!(back["items"][0]["type"], "supersedes");
    assert_eq!(back["items"][0]["origin"], "system");
    let sys = id(&back["items"][0]);
    let r = a
        .t(Method::DELETE, &format!("/api/v1/edges/{sys}"), None)
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());

    // axiom: рёбра от неё и к ней агент создаёт и удаляет сам (§8.5, решение владельца
    // 2026-10-26: ruling R10 снят); человеку (cookie) — тоже
    let r = a
        .adm(
            Method::POST,
            "/api/v1/nodes",
            Some(
                json!({"space": "knowledge", "path": "аксиома", "kind": "note",
                        "content": "---\ncertainty: axiom\n---\nистина\n"}),
            ),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let ax = id(&r.json());
    let r = edge(&a, &tok, &format!("n:{ax}"), "relates_to", &src).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let by_agent = id(&r.json());
    let r = a
        .t(Method::DELETE, &format!("/api/v1/edges/{by_agent}"), None)
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    let r = edge(&a, &tok, &src, "relates_to", &format!("n:{ax}")).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = a
        .adm(
            Method::POST,
            "/api/v1/edges",
            Some(json!({"src": format!("n:{ax}"), "type": "relates_to", "dst": src})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let from_ax = id(&r.json());
    let r = a
        .t(Method::DELETE, &format!("/api/v1/edges/{from_ax}"), None)
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());

    // links insight → явные рёбра; supersedes в links — 422, цель не найдена — 422
    let mut body = insight("вывод со связями", "три");
    body["links"] = json!([
        {"type": "derived_from", "target": format!("node:{x}")},
        {"type": "relates_to", "target": "[[Игрек]]"},
        {"type": "mentions", "target": format!("entry:{old_id}")},
    ]);
    let li = create_insight(&a, &tok, "knowledge", body).await;
    let out = get(&a, &tok, &format!("/api/v1/graph?entry={}", id(&li)))
        .await
        .json();
    assert_eq!(out["edges"].as_array().unwrap().len(), 3, "{out}");
    for (links, want) in [
        (
            json!([{"type": "supersedes", "target": format!("node:{x}")}]),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            json!([{"type": "relates_to", "target": "[[Нет такой]]"}]),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            json!([{"type": "relates_to"}]),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            json!([{"type": "relates_to", "target": format!("node:{}", zam_core::Id::new())}]),
            StatusCode::NOT_FOUND,
        ),
    ] {
        let mut body = insight("ещё вывод", "четыре");
        body["links"] = links.clone();
        let r = a
            .t(Method::POST, "/api/v1/spaces/knowledge/entries", Some(body))
            .await;
        assert_eq!(r.status, want, "{links}: {}", r.text());
    }
}

struct W {
    a: Api,
    a_tok: String,
    b_tok: String,
}

/// A регистрирует решение, B получает доступ `rw`.
async fn world(name: &str) -> W {
    let a = api(&format!("graph-{name}"), |_| {}).await;
    let (_, a_tok) = a.new_user("Анна").await;
    let (b_id, b_tok) = a.new_user("Борис").await;
    let reg = json!({"agent_solution_id": G, "name": "Общее"});
    let r = a
        .tok(&a_tok, Method::POST, "/api/v1/solutions", Some(reg.clone()))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = a
        .tok(&b_tok, Method::POST, "/api/v1/solutions", Some(reg))
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    let r = a
        .tok(
            &a_tok,
            Method::POST,
            &format!("/api/v1/solutions/{G}/access/{b_id}/approve"),
            Some(json!({})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    W { a, a_tok, b_tok }
}

#[tokio::test]
async fn cross_db_edge_422() {
    let w = world("cross-db").await;
    let s = id(&note_by(&w.a, &w.a_tok, G, "решение", "# Р\n").await);
    let mine = id(&note_by(&w.a, &w.b_tok, "knowledge", "моё", "# М\n").await);
    // участник: из решения (база владельца) в свою личную память и обратно — 422
    let r = edge(
        &w.a,
        &w.b_tok,
        &format!("n:{s}"),
        "relates_to",
        &format!("n:{mine}"),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    let r = edge(
        &w.a,
        &w.b_tok,
        &format!("n:{mine}"),
        "relates_to",
        &format!("n:{s}"),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    // в insight решения — тоже
    let mut body = insight("вывод участника", "тело");
    body["links"] = json!([{"type": "relates_to", "target": format!("node:{mine}")}]);
    let r =
        w.a.tok(
            &w.b_tok,
            Method::POST,
            &format!("/api/v1/spaces/{G}/entries"),
            Some(body),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    // внутри одной базы — можно
    let r = edge(
        &w.a,
        &w.b_tok,
        &format!("n:{s}"),
        "relates_to",
        &format!("n:{s}"),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "петля: {}", r.text());
    let s2 = id(&note_by(&w.a, &w.b_tok, G, "решение-2", "# Р2\n").await);
    let r = edge(
        &w.a,
        &w.b_tok,
        &format!("n:{s}"),
        "relates_to",
        &format!("n:{s2}"),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
}

#[tokio::test]
async fn member_graph_and_backlinks_hide_owner_other_spaces() {
    let w = world("member").await;
    let (a, at, bt) = (&w.a, w.a_tok.as_str(), w.b_tok.as_str());
    let k = id(&note_by(a, at, "knowledge", "секрет", "# Секрет владельца\n").await);
    let s = id(&note_by(
        a,
        at,
        G,
        "сборка",
        "# Сборка\n\nквазарный конвейер сборки\n",
    )
    .await);
    // владелец связывает insight решения со своим Knowledge (одна база — можно)
    let mut body = insight("квазарный вывод", "про конвейер");
    body["links"] = json!([
        {"type": "relates_to", "target": format!("node:{k}")},
        {"type": "derived_from", "target": format!("node:{s}")},
    ]);
    let ins = id(&create_insight(a, at, G, body).await);
    let r = edge(a, at, &format!("n:{k}"), "mentions", &format!("n:{s}")).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let k_to_s = id(&r.json());
    // владелец видит всё
    let j = get(a, at, &format!("/api/v1/graph?entry={ins}&depth=2"))
        .await
        .json();
    assert!(graph_refs(&j).contains(&format!("n:{k}")), "{j}");

    // участник: ни узлом, ни ребром, ни обратной ссылкой
    let r = get(a, bt, &format!("/api/v1/graph?entry={ins}&depth=3")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(
        graph_refs(&j),
        sorted(vec![format!("e:{ins}"), format!("n:{s}")])
    );
    let text = j.to_string();
    assert!(!text.contains(&k), "{text}");
    for uri in [
        format!("/api/v1/nodes/{s}/backlinks"),
        format!("/api/v1/entries/{ins}/backlinks"),
        format!("/api/v1/graph?node={s}&depth=3"),
    ] {
        let r = get(a, bt, &uri).await;
        assert_eq!(r.status, StatusCode::OK, "{uri}: {}", r.text());
        assert!(!r.text().contains(&k), "{uri}: {}", r.text());
    }
    let r = get(a, bt, &format!("/api/v1/graph?entry={ins}")).await;
    assert!(!r.text().contains(&k));
    // исходящие insight: связь в Knowledge отфильтрована
    let r = get(a, bt, &format!("/api/v1/nodes/{s}/links")).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(!r.text().contains(&k), "{}", r.text());
    // прямые обращения к Knowledge владельца — 404
    for uri in [
        format!("/api/v1/graph?node={k}"),
        format!("/api/v1/nodes/{k}/backlinks"),
        format!("/api/v1/nodes/{k}/links"),
    ] {
        assert_eq!(
            get(a, bt, &uri).await.status,
            StatusCode::NOT_FOUND,
            "{uri}"
        );
    }
    let r = edge(a, bt, &format!("n:{s}"), "relates_to", &format!("n:{k}")).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    let r = a
        .tok(bt, Method::DELETE, &format!("/api/v1/edges/{k_to_s}"), None)
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    let mut body = insight("вывод участника", "тело");
    body["links"] = json!([{"type": "relates_to", "target": format!("node:{k}")}]);
    let r = a
        .tok(
            bt,
            Method::POST,
            &format!("/api/v1/spaces/{G}/entries"),
            Some(body),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());

    // expand=graph: участнику сосед из Knowledge владельца не попадает, владельцу — попадает
    let q = json!({"query": "квазарный", "expand": "graph", "top_k": 10,
                   "scope": {"solution": G}});
    let r = a
        .tok(bt, Method::POST, "/api/v1/search", Some(q.clone()))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(!r.text().contains(&k), "{}", r.text());
    let r = a.tok(at, Method::POST, "/api/v1/search", Some(q)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let items = r.json()["items"].as_array().unwrap().clone();
    let ks: Vec<&Value> = items
        .iter()
        .filter(|i| i["node_id"] == k.as_str())
        .collect();
    assert_eq!(ks.len(), 1, "{items:?}");
    assert_eq!(ks[0]["sources"], json!(["graph"]));
}

#[tokio::test]
async fn expand_graph_adds_neighbors_with_graph_source() {
    let a = api("graph-expand", |_| {}).await;
    let tok = a.token.clone();
    let hit = id(&a
        .note(
            "knowledge",
            "хит",
            "# Хит\n\nпульсарный режим сборки, см. [[Сосед]]\n",
        )
        .await);
    let nb = id(&a
        .note(
            "knowledge",
            "Сосед",
            "# Сосед\n\nсоседняя заметка без слова запроса\n",
        )
        .await);
    // действующий insight и замещённый — оба связаны с найденной заметкой
    let mut body = insight("вывод о режиме", "соседний вывод");
    body["links"] = json!([{"type": "relates_to", "target": format!("node:{hit}")}]);
    let live = id(&create_insight(&a, &tok, "knowledge", body).await);
    let mut body = insight("устаревший вывод", "старое");
    body["links"] = json!([{"type": "relates_to", "target": format!("node:{hit}")}]);
    let gone = id(&create_insight(&a, &tok, "knowledge", body).await);
    let r = a
        .t(
            Method::POST,
            &format!("/api/v1/entries/{gone}/retract"),
            Some(json!({"reason": "ошибка"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    let search = |q: Value| a.t(Method::POST, "/api/v1/search", Some(q));
    // без expand — только хит
    let j = search(json!({"query": "пульсарный"})).await.json();
    assert_eq!(j["items"].as_array().unwrap().len(), 1, "{j}");
    // top_k 1 → не больше ceil(1/2) = 1 нового элемента: первым — действующий insight
    let r = search(json!({"query": "пульсарный", "expand": "graph", "top_k": 1})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    let items = j["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{j}");
    assert_eq!(items[0]["node_id"], hit.as_str());
    assert_eq!(items[1]["entry_id"], live.as_str());
    assert_eq!(items[1]["sources"], json!(["graph"]));
    assert_eq!(
        items[1]["via"],
        json!({"type": "relates_to", "from": format!("n:{hit}")})
    );
    let parent = items[0]["score"].as_f64().unwrap();
    let child = items[1]["score"].as_f64().unwrap();
    assert!((child - (parent * 0.5)).abs() < 1e-3, "{parent} {child}");
    // top_k 4 → до 2 новых: insight, затем заметка; отозванный — нет
    let j = search(json!({"query": "пульсарный", "expand": "graph", "top_k": 4, "mode": "chunks"}))
        .await
        .json();
    let items = j["items"].as_array().unwrap();
    let ids: Vec<String> = items
        .iter()
        .map(|i| {
            i["node_id"]
                .as_str()
                .or(i["entry_id"].as_str())
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(ids, vec![hit.clone(), live.clone(), nb.clone()], "{j}");
    assert_eq!(items[2]["via"]["type"], "links_to");
    assert!(
        items[2]["text"].as_str().unwrap().contains("соседняя"),
        "{j}"
    );
    // история — и отозванный
    let j = search(
        json!({"query": "пульсарный", "expand": "graph", "top_k": 10,
                          "include_history": true}),
    )
    .await
    .json();
    let all = j.to_string();
    assert!(all.contains(&gone), "{all}");
    // md-формат: пометка via
    let r =
        search(json!({"query": "пульсарный", "expand": "graph", "top_k": 1, "format": "md"})).await;
    assert!(r.text().contains("via relates_to"), "{}", r.text());
}

#[tokio::test]
async fn render_and_edges_agree_on_resolution() {
    let a = api("graph-render", |_| {}).await;
    let tok = a.token.clone();
    let late = id(&a.note("knowledge", "b/N", "# поздний\n").await);
    let src = id(&a.note("knowledge", "src", "[[N]] и [[Нет]]\n").await);
    let early = id(&a.note("knowledge", "a/N", "# ранний\n").await);
    let check = |want: String| {
        let a = &a;
        let tok = tok.clone();
        let src = src.clone();
        async move {
            let r = get(a, &tok, &format!("/api/v1/nodes/{src}?format=html")).await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text());
            let html = r.json()["html"].as_str().unwrap().to_string();
            assert!(html.contains(&format!("href=\"#/n/{want}\"")), "{html}");
            let links = get(a, &tok, &format!("/api/v1/nodes/{src}/links"))
                .await
                .json();
            let items = links["items"].as_array().unwrap();
            assert_eq!(items[0]["dst"], format!("n:{want}"), "{links}");
            assert_eq!(items[1]["dst"], Value::Null);
        }
    };
    // первая по пути — a/N, как в рендере
    check(early.clone()).await;
    let r = a
        .t(Method::DELETE, &format!("/api/v1/nodes/{early}"), None)
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    check(late.clone()).await;
    let r = a
        .t(
            Method::POST,
            &format!("/api/v1/nodes/{early}/restore"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    check(early).await;
}
