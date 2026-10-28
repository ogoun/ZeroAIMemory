//! Решения с обоснованием и противоречия (M4.5 §5, задача 6): ADR-поля `data.decision`,
//! фильтр `decided_by`, предупреждение о близких аксиомах (`decision_near_axiom`), связь
//! `contradicts` тремя путями и порядок выдачи (аксиома сразу перед записью, которая ей
//! противоречит, — после реранка). Серверы без моделей (FTS); проверка близких аксиом по
//! вектору — MiniLM на CPU-сборке ORT и настоящий Qdrant, без них тест пропускается (в CI,
//! `ZAM_REQUIRE_ML=1`, — падает). Реранкер — детерминированная заглушка.
mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::http::{Method, StatusCode};
use common::api::{Api, Resp, api};
use common::ml::{MINILM, external, have, ml, until, vector_ready};
use common::qdrant::TestQdrant;
use serde_json::{Value, json};
use zam_core::Id;
use zam_ml::MlError;
use zam_server::config::ServerConfig;
use zam_server::rerank::PassageScorer;
use zam_store::{IndexState, get_entry};

const KN: &str = "/api/v1/spaces/knowledge/entries";
/// Предел ожидания индексации (инференс на CPU, запись в Qdrant на HDD).
const INDEXED: Duration = Duration::from_secs(240);

fn id(v: &Value) -> String {
    v["id"]
        .as_str()
        .unwrap_or_else(|| panic!("нет id: {v}"))
        .to_string()
}

/// Решение `title` с телом `body`, `data.decision` (если задано) и `extra` поверх.
fn decision(title: &str, body: &str, adr: Option<Value>, extra: Value) -> Value {
    let mut data = json!({"kind": "decision"});
    if let Some(adr) = adr {
        data["decision"] = adr;
    }
    let mut v = json!({"kind": "insight", "title": title, "body": body, "data": data});
    for (k, x) in extra.as_object().unwrap() {
        v[k] = x.clone();
    }
    v
}

/// Запись агентом (токен): ответ.
async fn post(a: &Api, body: Value) -> Resp {
    a.t(Method::POST, KN, Some(body)).await
}

/// Запись человеком (сессия WebUI).
async fn post_human(a: &Api, body: Value) -> Resp {
    a.adm(Method::POST, KN, Some(body), None).await
}

async fn created(r: Resp) -> Value {
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()
}

fn warnings(v: &Value) -> Vec<String> {
    v["warnings"]
        .as_array()
        .map(|a| a.iter().map(|w| w.as_str().unwrap().to_string()).collect())
        .unwrap_or_default()
}

fn unprocessable(r: &Resp, needle: &str) {
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    assert!(r.text().contains(needle), "нет «{needle}»: {}", r.text());
}

#[tokio::test]
async fn decision_adr_fields_validated() {
    let a = api("decisions-adr", |_| {}).await;
    let adr = json!({
        "context": "База растёт", "options": ["SQLite", "Postgres"], "chosen": "SQLite",
        "rationale": "Одна машина", "consequences": "Нет репликации",
        "decided_by": "agent", "revisit_if": "Появится второй узел"
    });
    let e = created(
        post(
            &a,
            decision("ADR: хранилище", "b", Some(adr.clone()), json!({})),
        )
        .await,
    )
    .await;
    assert_eq!(e["data"]["decision"], adr);

    // все поля необязательны; null — как отсутствие
    created(post(&a, decision("ADR: пустой", "b", Some(json!({})), json!({}))).await).await;
    created(
        post(
            &a,
            decision(
                "ADR: null",
                "b",
                Some(json!({"chosen": null, "options": null})),
                json!({}),
            ),
        )
        .await,
    )
    .await;

    // неизвестный ключ, типы, пределы
    let long = "я".repeat(2001);
    let nine: Vec<String> = (0..9).map(|i| format!("в{i}")).collect();
    let cases = [
        (json!({"foo": 1}), "data.decision.foo: неизвестное поле"),
        (json!({"context": 5}), "data.decision.context"),
        (json!({"chosen": long}), "data.decision.chosen"),
        (json!({"options": nine}), "data.decision.options"),
        (json!({"options": "SQLite"}), "data.decision.options"),
        (json!({"options": [1]}), "data.decision.options"),
        (json!({"decided_by": "robot"}), "data.decision.decided_by"),
        (json!("текст"), "data.decision"),
    ];
    for (i, (bad, needle)) in cases.into_iter().enumerate() {
        let r = post(
            &a,
            decision(&format!("ADR: плохой {i}"), "b", Some(bad), json!({})),
        )
        .await;
        unprocessable(&r, needle);
    }

    // ровно 8 вариантов и ровно 2000 символов — можно
    let ok = json!({"options": (0..8).map(|i| format!("в{i}")).collect::<Vec<_>>(),
                    "rationale": "я".repeat(2000)});
    created(post(&a, decision("ADR: предел", "b", Some(ok), json!({}))).await).await;

    // проверяется только у decision: у gotcha ключ `decision` — произвольные данные
    let g = json!({"kind": "insight", "title": "Не решение", "body": "b",
                   "data": {"kind": "gotcha", "decision": {"foo": 1}}});
    created(post(&a, g).await).await;

    // замещение проверяет так же
    let r = a
        .t(
            Method::POST,
            &format!("/api/v1/entries/{}/supersede", id(&e)),
            Some(decision(
                "ADR: хранилище v2",
                "b",
                Some(json!({"foo": 1})),
                json!({}),
            )),
        )
        .await;
    unprocessable(&r, "data.decision.foo: неизвестное поле");
}

#[tokio::test]
async fn decided_by_filter() {
    let a = api("decisions-by", |_| {}).await;
    let adr = |by: &str| Some(json!({"decided_by": by}));
    // человек без поля, человек с полем, агент с полем, агент без поля (по автору записи — agent)
    let h1 = created(
        post_human(
            &a,
            decision("Решение кэша первое", "кэш сборки", None, json!({})),
        )
        .await,
    )
    .await;
    let h2 = created(
        post_human(
            &a,
            decision("Решение кэша второе", "кэш сборки", adr("human"), json!({})),
        )
        .await,
    )
    .await;
    let a1 = created(
        post(
            &a,
            decision("Решение кэша третье", "кэш сборки", adr("agent"), json!({})),
        )
        .await,
    )
    .await;
    let a2 = created(
        post(
            &a,
            decision("Решение кэша четвёртое", "кэш сборки", None, json!({})),
        )
        .await,
    )
    .await;
    // не решение с тем же полем и обычный gotcha — под фильтр не попадают
    let g = json!({"kind": "insight", "title": "Кэш сборки: ловушка", "body": "кэш сборки",
                   "data": {"kind": "gotcha"}});
    created(post(&a, g).await).await;

    let ids = |j: &Value| -> Vec<String> {
        let mut v: Vec<String> = j["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|i| {
                i["entry_id"]
                    .as_str()
                    .or(i["id"].as_str())
                    .map(String::from)
            })
            .collect();
        v.sort();
        v
    };
    let sorted = |v: &[&Value]| -> Vec<String> {
        let mut v: Vec<String> = v.iter().map(|e| id(e)).collect();
        v.sort();
        v
    };
    let search = |by: Option<&str>| {
        let mut body = json!({"query": "кэш сборки", "top_k": 20});
        if let Some(by) = by {
            body["filters"] = json!({"decided_by": by});
        }
        let a = &a;
        async move {
            let r = a.t(Method::POST, "/api/v1/search", Some(body)).await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text());
            r.json()
        }
    };
    assert_eq!(ids(&search(Some("agent")).await), sorted(&[&a1, &a2]));
    assert_eq!(ids(&search(Some("human")).await), sorted(&[&h1, &h2]));
    assert_eq!(ids(&search(None).await).len(), 5);
    let r = a
        .t(
            Method::POST,
            "/api/v1/search",
            Some(json!({"query": "кэш", "filters": {"decided_by": "robot"}})),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());

    // лента записей
    let feed = |q: &str| {
        let uri = format!("{KN}?{q}");
        let a = &a;
        async move {
            let r = a.t(Method::GET, &uri, None).await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text());
            r.json()
        }
    };
    assert_eq!(ids(&feed("decided_by=agent").await), sorted(&[&a1, &a2]));
    assert_eq!(ids(&feed("decided_by=human").await), sorted(&[&h1, &h2]));
    assert_eq!(ids(&feed("kind=insight").await).len(), 5);
    let r = a
        .t(Method::GET, &format!("{KN}?decided_by=robot"), None)
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
}

/// Без модели: близкие аксиомы — по нормализованному заголовку и `decision_check: degraded`.
#[tokio::test]
async fn decision_check_degraded_without_model() {
    let a = api("decisions-degraded", |_| {}).await;
    assert!(a.st.index.vector_available().is_err());
    let ax = created(
        post(
            &a,
            decision(
                "Хранить данные в SQLite",
                "Одна машина",
                None,
                json!({"certainty": "axiom"}),
            ),
        )
        .await,
    )
    .await;
    let near = format!(
        "decision_near_axiom: i:{} «Хранить данные в SQLite»",
        id(&ax)
    );
    let degraded = "decision_check: degraded";

    // тот же заголовок (force обходит дубль) — предупреждение и режим degraded; запись создана
    let e = created(
        post(
            &a,
            decision(
                " хранить данные  в sqlite! ",
                "Ещё раз",
                None,
                json!({"force": true}),
            ),
        )
        .await,
    )
    .await;
    assert_eq!(warnings(&e), [near.as_str(), degraded], "{e}");

    // другой заголовок — только degraded
    let e = created(post(&a, decision("Выбираем цвет кнопки", "x", None, json!({}))).await).await;
    assert_eq!(warnings(&e), [degraded], "{e}");

    // замещение проходит проверку так же
    let r = a
        .t(
            Method::POST,
            &format!("/api/v1/entries/{}/supersede", id(&e)),
            Some(decision("Хранить данные в SQLite", "v2", None, json!({}))),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(warnings(&r.json()["entry"]), [near.as_str(), degraded]);

    // другая область аксиомы — не близкая
    let e = created(
        post(
            &a,
            decision(
                "Хранить данные в SQLite",
                "x",
                None,
                json!({"area": "db", "force": true}),
            ),
        )
        .await,
    )
    .await;
    assert_eq!(warnings(&e), [degraded], "{e}");

    // человек (WebUI) и не-решение проверки не проходят
    let e = created(
        post_human(
            &a,
            decision("Хранить данные в SQLite", "x", None, json!({"force": true})),
        )
        .await,
    )
    .await;
    assert!(warnings(&e).is_empty(), "{e}");
    let g = json!({"kind": "insight", "title": "Хранить данные в SQLite", "body": "x",
                   "force": true, "data": {"kind": "gotcha"}});
    let e = created(post(&a, g).await).await;
    assert!(warnings(&e).is_empty(), "{e}");

    // не аксиома с тем же заголовком — не близкая аксиома: теорема/гипотеза не считаются
    let e = created(
        post(
            &a,
            decision("Гипотеза про индекс", "x", None, json!({"area": "idx"})),
        )
        .await,
    )
    .await;
    let e2 = created(
        post(
            &a,
            decision(
                "Гипотеза про индекс",
                "y",
                None,
                json!({"area": "idx", "force": true}),
            ),
        )
        .await,
    )
    .await;
    assert_eq!(warnings(&e2), [degraded], "{e} {e2}");
}

/// С моделью: близкая по смыслу аксиома-решение (до 3) находится по dense-косинусу.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn agent_decision_near_axiom_warns() {
    if !have(MINILM) {
        return;
    }
    let q = TestQdrant::start("dec-near");
    let a = api("dec-near", |c| {
        ml(c, MINILM);
        external(c, &q);
    })
    .await;
    vector_ready(&a).await;
    let ax = created(
        post(
            &a,
            decision(
                "Store application data in SQLite",
                "We keep all application data in a single SQLite database file on one machine. \
                 PostgreSQL is not used.",
                None,
                json!({"certainty": "axiom"}),
            ),
        )
        .await,
    )
    .await;
    assert_eq!(ax["dedup"], "full");
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    let axid: Id = id(&ax).parse().unwrap();
    a.st.index.worker.notify();
    until(
        "аксиома проиндексирована",
        INDEXED,
        async || get_entry(&db, axid).await.unwrap().index_state == IndexState::Indexed,
    )
    .await;

    // близкий по смыслу, другой заголовок — предупреждение без degraded
    let e = created(
        post(
            &a,
            decision(
                "Use SQLite as the storage engine",
                "Application data will be kept in one SQLite database file. We do not need \
                 PostgreSQL.",
                None,
                json!({"force": true}),
            ),
        )
        .await,
    )
    .await;
    let w = warnings(&e);
    assert_eq!(
        w,
        [format!(
            "decision_near_axiom: i:{} «Store application data in SQLite»",
            id(&ax)
        )],
        "{e}"
    );
    // другой смысл — предупреждений нет
    let e = created(
        post(
            &a,
            decision(
                "Button color is blue",
                "The primary button in the web interface is painted blue.",
                None,
                json!({}),
            ),
        )
        .await,
    )
    .await;
    assert!(warnings(&e).is_empty(), "{e}");
    // человек в WebUI предупреждений не получает
    let e = created(
        post_human(
            &a,
            decision(
                "Use SQLite as the storage engine today",
                "Application data will be kept in one SQLite database file. We do not need \
                 PostgreSQL.",
                None,
                json!({"force": true}),
            ),
        )
        .await,
    )
    .await;
    assert!(warnings(&e).is_empty(), "{e}");
}

#[tokio::test]
async fn contradicts_edge_via_links_and_relations() {
    let a = api("decisions-edges", |_| {}).await;
    let ax = created(
        post(
            &a,
            decision("Аксиома про кэш", "x", None, json!({"certainty": "axiom"})),
        )
        .await,
    )
    .await;
    let ax_ref = format!("entry:{}", id(&ax));

    // 1) links записи
    let h1 = created(
        post(
            &a,
            decision(
                "Гипотеза: кэш не нужен",
                "x",
                None,
                json!({"links": [{"type": "contradicts", "target": ax_ref}]}),
            ),
        )
        .await,
    )
    .await;
    // 2) POST /edges
    let h2 = created(post(&a, decision("Гипотеза: кэш вреден", "x", None, json!({}))).await).await;
    let r = a
        .t(
            Method::POST,
            "/api/v1/edges",
            Some(
                json!({"src": format!("entry:{}", id(&h2)), "type": "contradicts",
                        "dst": ax_ref}),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(r.json()["type"], "contradicts");

    let g = a
        .t(
            Method::GET,
            &format!("/api/v1/graph?entry={}&depth=1&types=contradicts", id(&ax)),
            None,
        )
        .await;
    assert_eq!(g.status, StatusCode::OK, "{}", g.text());
    let g = g.json();
    let edges = g["edges"].as_array().unwrap();
    assert_eq!(edges.len(), 2, "{g}");
    let srcs: Vec<String> = edges
        .iter()
        .map(|e| {
            assert_eq!(
                (&e["type"], &e["origin"]),
                (&json!("contradicts"), &json!("explicit"))
            );
            e["src"].to_string()
        })
        .collect();
    for h in [&h1, &h2] {
        assert!(srcs.iter().any(|s| s.contains(&id(h))), "{g}");
    }

    // 3) relations: заметки (frontmatter); тип принят — предупреждения нет
    let base = a.note("knowledge", "base", "# База\n").await;
    let r = a
        .t(
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": "knowledge", "path": "claim", "kind": "note",
                        "content": "---\nrelations: [\"contradicts: [[base]]\"]\n---\n# Утверждение\n"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert!(r.json().get("warnings").is_none(), "{}", r.text());
    let g = a
        .t(
            Method::GET,
            &format!("/api/v1/graph?node={}&depth=1&types=contradicts", id(&base)),
            None,
        )
        .await
        .json();
    let edges = g["edges"].as_array().unwrap();
    assert_eq!(edges.len(), 1, "{g}");
    assert_eq!(
        (&edges[0]["type"], &edges[0]["origin"]),
        (&json!("contradicts"), &json!("frontmatter"))
    );
}

/// Заглушка реранкера: балл фрагмента — по первой подходящей метке, иначе 0,1.
struct Stub(Vec<(&'static str, f32)>);

impl PassageScorer for Stub {
    fn model_id(&self) -> &str {
        "stub-reranker"
    }

    fn score_capped(&self, _q: &str, passages: &[&str], _t: usize) -> Result<Vec<f32>, MlError> {
        Ok(passages
            .iter()
            .map(|p| {
                self.0
                    .iter()
                    .find(|(m, _)| p.contains(m))
                    .map_or(0.1, |(_, s)| *s)
            })
            .collect())
    }
}

/// Сервер с аксиомой и гипотезой, обе находятся запросом «сборка кэша»; реранкер ставит
/// гипотезу (0,90) выше аксиомы (0,50 × 1,20 = 0,60). Возвращает сервер и id гипотезы и аксиомы.
async fn rerank_world(name: &str, tweak: impl FnOnce(&mut ServerConfig)) -> (Api, String, String) {
    let a = api(name, tweak).await;
    a.st.rerank
        .install(Arc::new(Stub(vec![
            ("маркергипотезы", 0.90),
            ("маркераксиомы", 0.50),
        ])))
        .await;
    let ax = created(
        post(
            &a,
            decision(
                "Сборка кэша: аксиома",
                "Сборка кэша идёт без прогрева, маркераксиомы.",
                None,
                json!({"certainty": "axiom"}),
            ),
        )
        .await,
    )
    .await;
    let hyp = created(
        post(
            &a,
            decision(
                "Сборка кэша: гипотеза",
                "Сборка кэша ускоряется прогревом, маркергипотезы.",
                None,
                json!({"certainty": "hypothesis"}),
            ),
        )
        .await,
    )
    .await;
    (a, id(&hyp), id(&ax))
}

async fn rerank_order(a: &Api) -> (Vec<String>, Vec<f64>) {
    let r = a
        .t(
            Method::POST,
            "/api/v1/search",
            Some(json!({"query": "сборка кэша", "rerank": "on"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["warnings"], json!([]), "{j}");
    let items = j["items"].as_array().unwrap();
    (
        items
            .iter()
            .map(|i| i["entry_id"].as_str().unwrap().to_string())
            .collect(),
        items.iter().map(|i| i["score"].as_f64().unwrap()).collect(),
    )
}

async fn contradict(a: &Api, hyp: &str, ax: &str) {
    let r = a
        .t(
            Method::POST,
            "/api/v1/edges",
            Some(json!({"src": format!("entry:{hyp}"), "type": "contradicts",
                        "dst": format!("entry:{ax}")})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
}

#[tokio::test]
async fn contradicting_hypothesis_not_above_axiom_rerank() {
    let (a, hyp, ax) = rerank_world("decisions-rerank", |_| {}).await;
    // без связи реранкер ставит гипотезу первой
    let (ids, _) = rerank_order(&a).await;
    assert_eq!(ids, [hyp.clone(), ax.clone()]);
    // гипотеза противоречит аксиоме — аксиома сразу перед ней, баллы не идут вразрез с порядком
    contradict(&a, &hyp, &ax).await;
    let (ids, scores) = rerank_order(&a).await;
    assert_eq!(ids, [ax.clone(), hyp.clone()]);
    assert!(scores[0] >= scores[1], "{scores:?}");
    // связь в обратную сторону (аксиома → гипотеза) порядка не меняет
    let (a, hyp, ax) = rerank_world("decisions-rerank-back", |_| {}).await;
    contradict(&a, &ax, &hyp).await;
    let (ids, _) = rerank_order(&a).await;
    assert_eq!(ids, [hyp, ax]);
}

#[tokio::test]
async fn contradicts_order_off_keeps_order() {
    let (a, hyp, ax) = rerank_world("decisions-rerank-off", |c| {
        c.search.contradicts_order = false;
    })
    .await;
    contradict(&a, &hyp, &ax).await;
    let (ids, _) = rerank_order(&a).await;
    assert_eq!(ids, [hyp, ax], "порядок реранкера без поправки");
}
