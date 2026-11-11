//! Бусты после серверного реранка (§9.2 п. 5, решение владельца 2026-10-26): у переставленных
//! реранкером `score = rerank_score × boost_kind × boost_area × boost_certainty`, затем деление
//! при переполнении; `rerank_score` в ответе — сырой балл реранкера. Реранкер — детерминированная
//! заглушка (`RerankSlot::install`): балл по метке в тексте фрагмента. Без моделей (FTS).

mod common;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use common::api::{Api, api};
use serde_json::{Value, json};
use zam_ml::MlError;
use zam_search::KindBoostMode;
use zam_server::rerank::PassageScorer;

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

/// Сервер с двумя выводами: `hypothesis` (метка «маркергипотезы») и `theorem` с доказательством
/// (метка «маркертеоремы»); оба находятся запросом «сборка кэша».
async fn setup(name: &str, marks: Vec<(&'static str, f32)>) -> Api {
    // без отсечки слабых карточек (M6 §4.2): тесты — о баллах реранка
    let a = api(name, |c| c.search.brief_min_rel = 0.0).await;
    a.st.rerank.install(Arc::new(Stub(marks))).await;
    for (title, body, certainty) in [
        (
            "Сборка кэша: гипотеза",
            "Сборка кэша ускоряется прогревом, маркергипотезы.",
            "hypothesis",
        ),
        (
            "Сборка кэша: теорема",
            "Сборка кэша ускоряется прогревом, маркертеоремы.",
            "theorem",
        ),
    ] {
        let mut data = json!({"kind": "discovery"});
        if certainty == "theorem" {
            data["evidence"] = json!({"checked": "сборка на CI", "method": "прогон 3 раза",
                                      "result": "воспроизводится"});
        }
        let r = a
            .t(
                Method::POST,
                "/api/v1/spaces/knowledge/entries",
                Some(
                    json!({"kind": "insight", "title": title, "body": body, "data": data,
                            "certainty": certainty, "force": true}),
                ),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    }
    a
}

async fn search(a: &Api, body: Value) -> Value {
    let r = a.t(Method::POST, "/api/v1/search", Some(body)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

fn item<'a>(j: &'a Value, certainty: &str) -> &'a Value {
    j["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["certainty"] == certainty)
        .unwrap_or_else(|| panic!("нет {certainty}: {j}"))
}

/// Гипотеза с бо́льшим баллом реранкера (0,80 × 1,00) уступает теореме с меньшим
/// (0,75 × 1,10 = 0,825): порядок — по произведению.
#[tokio::test]
async fn rerank_respects_certainty_boost() {
    let a = setup(
        "rr-certainty",
        vec![("маркергипотезы", 0.80), ("маркертеоремы", 0.75)],
    )
    .await;
    let j = search(&a, json!({"query": "сборка кэша", "rerank": "on"})).await;
    assert_eq!(j["warnings"], json!([]), "{j}");
    let order: Vec<&str> = j["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["certainty"].as_str().unwrap())
        .collect();
    assert_eq!(order, ["theorem", "hypothesis"], "{j}");
    let (thm, hyp) = (item(&j, "theorem"), item(&j, "hypothesis"));
    assert_eq!(thm["rerank_score"], json!(0.75), "{j}");
    assert_eq!(thm["score"], json!(0.825), "{j}");
    assert_eq!(hyp["rerank_score"], json!(0.8), "{j}");
    assert_eq!(hyp["score"], json!(0.8), "{j}");
    // без реранка — тот же буст к слитому баллу: теорема тоже первая
    let j = search(&a, json!({"query": "сборка кэша"})).await;
    assert_eq!(j["items"][0]["certainty"], "theorem", "{j}");
    assert!(j["items"][0].get("rerank_score").is_none(), "{j}");
}

/// `rerank_score` — сырой балл реранкера, `score` — с бустами и делением при переполнении:
/// 0,95 × 1,10 = 1,045 > 1 → у лучшего `score` 1, у второго 0,5 / 1,045. Карточка `md` — с
/// баллом `score`.
#[tokio::test]
async fn rerank_score_raw_in_output() {
    let a = setup(
        "rr-raw",
        vec![("маркергипотезы", 0.50), ("маркертеоремы", 0.95)],
    )
    .await;
    let j = search(&a, json!({"query": "сборка кэша", "rerank": "on"})).await;
    let (thm, hyp) = (item(&j, "theorem"), item(&j, "hypothesis"));
    assert_eq!(j["items"][0]["certainty"], "theorem", "{j}");
    assert_eq!(thm["rerank_score"], json!(0.95), "{j}");
    assert_eq!(thm["score"], json!(1.0), "{j}");
    assert_eq!(hyp["rerank_score"], json!(0.5), "{j}");
    assert_eq!(hyp["score"], json!(0.4785), "{j}");
    let r = a
        .t(
            Method::POST,
            "/api/v1/search",
            Some(json!({"query": "сборка кэша", "rerank": "on", "format": "md"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let md = r.text();
    assert!(md.contains("[1.0]") && md.contains("[.48]"), "{md}");
}

// ---- режим буста вида `search.kind_boost_mode` (M6 §10, задача 10) ----

/// Эталон выдачи мира [`kind_world`] при `always × 1,05` (до правки задачи 10, шаг 0).
const KIND_BOOST_FIXTURE: &str = "tests/fixtures/kind_boost_m57.json";

/// Записи мира буста вида: (имя, вид insight, `certainty`, метка реранкера, балл реранкера).
/// Все находятся запросом «сборка кэша»; `hyp` противоречит `thm` (`contradicts`).
const KIND_WORLD: &[(&str, &str, &str, &str, f32)] = &[
    ("hyp", "gotcha", "hypothesis", "маркергипотезы", 0.80),
    ("thm", "discovery", "theorem", "маркертеоремы", 0.30),
    ("grab", "gotcha", "hypothesis", "маркерграбли", 0.60),
    ("fail", "failure", "theorem", "маркерсбоя", 0.55),
    ("ax", "gotcha", "axiom", "маркераксиомы", 0.50),
    ("disc", "discovery", "hypothesis", "маркернаблюдения", 0.82),
];

/// Сервер с записями [`KIND_WORLD`] и связью `hyp → thm` (`contradicts`); `tweak` — поправки
/// конфигурации поверх отсечки 0. Возвращает сервер и пары (id записи, имя).
async fn kind_world(
    name: &str,
    tweak: impl FnOnce(&mut zam_server::config::ServerConfig),
) -> (Api, Vec<(String, &'static str)>) {
    let a = api(name, |c| {
        c.search.brief_min_rel = 0.0;
        tweak(c);
    })
    .await;
    let marks = KIND_WORLD.iter().map(|w| (w.3, w.4)).collect();
    a.st.rerank.install(Arc::new(Stub(marks))).await;
    let mut ids = Vec::new();
    for &(key, kind, certainty, mark, _) in KIND_WORLD {
        let mut data = json!({"kind": kind});
        if certainty != "hypothesis" {
            data["evidence"] = json!({"checked": "сборка на CI", "method": "прогон 3 раза",
                                      "result": "воспроизводится"});
        }
        let body = json!({"kind": "insight", "title": format!("Сборка кэша: {key}"),
                          "body": format!("Сборка кэша ускоряется прогревом, {mark}."),
                          "data": data, "certainty": certainty, "force": true});
        let uri = "/api/v1/spaces/knowledge/entries";
        // аксиому пишет только человек (WebUI)
        let r = if certainty == "axiom" {
            a.adm(Method::POST, uri, Some(body), None).await
        } else {
            a.t(Method::POST, uri, Some(body)).await
        };
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
        ids.push((r.json()["id"].as_str().unwrap().to_string(), key));
    }
    let id = |k: &str| ids.iter().find(|(_, n)| *n == k).unwrap().0.clone();
    let r = a
        .t(
            Method::POST,
            "/api/v1/edges",
            Some(
                json!({"src": format!("e:{}", id("hyp")), "type": "contradicts",
                        "dst": format!("e:{}", id("thm"))}),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    (a, ids)
}

/// Выдача мира в сравнимом виде: по запросу — элементы `[имя, score, rerank_score, sources]`.
async fn kind_answers(a: &Api, ids: &[(String, &str)]) -> Value {
    let mut out = serde_json::Map::new();
    for rerank in ["off", "on"] {
        for top_k in [1, 3, 10] {
            let j = search(
                a,
                json!({"query": "сборка кэша", "rerank": rerank, "top_k": top_k}),
            )
            .await;
            let items: Vec<Value> = j["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| {
                    let e = i["entry_id"].as_str().unwrap_or_default();
                    let name = ids.iter().find(|(id, _)| id == e).map_or("?", |x| x.1);
                    json!([name, i["score"], i["rerank_score"], i["sources"]])
                })
                .collect();
            out.insert(format!("rerank={rerank},top_k={top_k}"), json!(items));
        }
    }
    Value::Object(out)
}

/// Снимает эталон [`KIND_BOOST_FIXTURE`] (код до задачи 10: буст вида всегда, 1,05).
#[tokio::test]
#[ignore = "снимает эталон; запускать только на коде до правки M6 §10"]
async fn snapshot_kind_boost_fixture() {
    let (a, ids) = kind_world("kb-snap", |_| {}).await;
    let v = kind_answers(&a, &ids).await;
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(KIND_BOOST_FIXTURE);
    std::fs::write(&path, serde_json::to_string_pretty(&v).unwrap() + "\n").unwrap();
}

/// `kind_boost_mode = always` (умолчание v0.0.9) — выдача побайтово как эталон до задачи 10.
#[tokio::test]
async fn always_byte_equal_m57() {
    let want: Value = serde_json::from_str(
        &std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(KIND_BOOST_FIXTURE),
        )
        .unwrap(),
    )
    .unwrap();
    let (a, ids) = kind_world("kb-always", |c| {
        c.search.kind_boost_mode = KindBoostMode::Always;
    })
    .await;
    assert_eq!(kind_answers(&a, &ids).await, want);
}

/// Порядок имён выдачи и баллы по имени.
fn named(j: &Value, ids: &[(String, &str)]) -> Vec<(String, f64)> {
    j["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            let e = i["entry_id"].as_str().unwrap_or_default();
            let n = ids.iter().find(|(id, _)| id == e).map_or("?", |x| x.1);
            (n.to_string(), i["score"].as_f64().unwrap())
        })
        .collect()
}

fn score_of(v: &[(String, f64)], name: &str) -> f64 {
    v.iter()
        .find(|(n, _)| n == name)
        .unwrap_or_else(|| panic!("нет {name}: {v:?}"))
        .1
}

/// `unless_contradicted`: гипотеза `hyp` (`gotcha`) теряет буст вида, потому что ей противоречит
/// теорема `thm` **среди кандидатов**, хотя `thm` отсечена `top_k = 1` (не видна). Реранкер
/// (заглушка): `hyp` 0,80, `disc` 0,82 — при `always` первой стоит `hyp` (0,80 × 1,05 = 0,84),
/// при `unless_contradicted` — `disc`. Отозванная теорема не кандидат — буст возвращается. Без
/// реранка — тот же набор: отношение баллов `hyp`/`disc` меньше ровно в 1,05 раза.
#[tokio::test]
async fn unless_contradicted_sees_cut_candidates() {
    let one = json!({"query": "сборка кэша", "rerank": "on", "top_k": 1});
    let (a, ids) = kind_world("kb-uc-always", |_| {}).await;
    let j = search(&a, one.clone()).await;
    assert_eq!(named(&j, &ids)[0].0, "hyp", "{j}");
    let fused_always = named(
        &search(&a, json!({"query": "сборка кэша", "top_k": 10})).await,
        &ids,
    );

    let (a, ids) = kind_world("kb-uc", |c| {
        c.search.kind_boost_mode = KindBoostMode::UnlessContradicted;
    })
    .await;
    let j = search(&a, one.clone()).await;
    let got = named(&j, &ids);
    assert_eq!(got.len(), 1, "{j}");
    assert_eq!(got[0].0, "disc", "{j}");
    // без отсечения: hyp — без буста вида (0,80), прочие gotcha — с ним
    let j = search(
        &a,
        json!({"query": "сборка кэша", "rerank": "on", "top_k": 10}),
    )
    .await;
    let all = named(&j, &ids);
    assert!((score_of(&all, "hyp") - 0.80).abs() < 1e-3, "{j}");
    assert!((score_of(&all, "grab") - 0.63).abs() < 1e-3, "{j}");
    // слитый балл (без реранка): тот же набор
    let fused = named(
        &search(&a, json!({"query": "сборка кэша", "top_k": 10})).await,
        &ids,
    );
    let ratio = |v: &[(String, f64)]| score_of(v, "hyp") / score_of(v, "disc");
    assert!(
        (ratio(&fused_always) / ratio(&fused) - 1.05).abs() < 2e-3,
        "{fused_always:?} / {fused:?}"
    );
    // отозванная теорема — не кандидат: буст гипотезы возвращается
    let thm = ids.iter().find(|(_, n)| *n == "thm").unwrap().0.clone();
    let r = a
        .t(
            Method::POST,
            &format!("/api/v1/entries/{thm}/retract"),
            Some(json!({"reason": "опровергнута"})),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    let j = search(&a, one).await;
    assert_eq!(named(&j, &ids)[0].0, "hyp", "{j}");
}

/// `reliable_only`: буст вида только у `theorem`/`axiom` (`fail` — `failure` `theorem`, `ax` —
/// `gotcha` `axiom`), у гипотез `hyp`, `grab` его нет; бусты `certainty` — как прежде.
#[tokio::test]
async fn reliable_only_boosts_theorem_axiom() {
    let (a, ids) = kind_world("kb-ro", |c| {
        c.search.kind_boost_mode = KindBoostMode::ReliableOnly;
    })
    .await;
    let j = search(
        &a,
        json!({"query": "сборка кэша", "rerank": "on", "top_k": 10}),
    )
    .await;
    let v = named(&j, &ids);
    for (name, want) in [
        ("hyp", 0.80),
        ("grab", 0.60),
        ("disc", 0.82),
        ("fail", 0.55 * 1.05 * 1.10),
        ("ax", 0.50 * 1.05 * 1.20),
        // `thm` (`discovery`, без буста вида): 0,30 × 1,10, но порядок по `contradicts` ставит
        // её перед `hyp` с баллом `hyp`
        ("thm", 0.80),
    ] {
        assert!((score_of(&v, name) - want).abs() < 1e-3, "{name}: {j}");
    }
}

/// Ревью задачи 10 (передача в задачу 16 M6): подграф `contradicts` кандидатов с бустом вида
/// обрезан пределом `GRAPH_MAX_NODES` — связь сверх предела не видна. Молча это не проходит:
/// в ответе предупреждение `contradicts graph truncated`; исключения, найденные в пределах
/// (гипотеза `hyp` против теоремы `thm`), действуют — выдача та же, что до обрезки. Граф читают
/// два шага — буст вида и порядок по `contradicts` (включён по умолчанию): предупреждение одно.
/// При `always` граф читает только порядок по `contradicts` — предупреждение тоже есть.
#[tokio::test]
async fn unless_contradicted_warns_on_truncated_graph() {
    use zam_server::api::search::graph::WARN_KIND_GRAPH_TRUNCATED;
    let (a, ids) = kind_world("kb-uc-trunc", |c| {
        c.search.kind_boost_mode = KindBoostMode::UnlessContradicted;
    })
    .await;
    let q = json!({"query": "сборка кэша", "rerank": "off", "top_k": 10});
    let warned = |j: &Value| {
        j["warnings"]
            .as_array()
            .unwrap()
            .contains(&json!(WARN_KIND_GRAPH_TRUNCATED))
    };
    let before = search(&a, q.clone()).await;
    assert!(!warned(&before), "{before}");
    // соседей `hyp` по `contradicts` — больше предела подграфа; запросом они не находятся
    let hyp = ids.iter().find(|(_, n)| *n == "hyp").unwrap().0.clone();
    for n in 0..=zam_store::GRAPH_MAX_NODES {
        let r = a
            .t(
                Method::POST,
                "/api/v1/spaces/knowledge/entries",
                Some(
                    json!({"kind": "insight", "title": format!("Посторонняя запись {n}"),
                            "body": format!("Наполнитель графа номер {n}."),
                            "data": {"kind": "discovery"}, "force": true}),
                ),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
        let id = r.json()["id"].as_str().unwrap().to_string();
        let r = a
            .t(
                Method::POST,
                "/api/v1/edges",
                Some(json!({"src": format!("e:{hyp}"), "type": "contradicts",
                            "dst": format!("e:{id}")})),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    }
    let after = search(&a, q.clone()).await;
    assert!(warned(&after), "{after}");
    assert_eq!(named(&after, &ids), named(&before, &ids));
    let count = |j: &Value| {
        j["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|w| **w == json!(WARN_KIND_GRAPH_TRUNCATED))
            .count()
    };
    assert_eq!(count(&after), 1, "без дублей: {after}");
    // тот же каталог данных, `kind_boost_mode = always`: граф читает только порядок по contradicts
    let mut cfg = a.st.cfg.as_ref().clone();
    cfg.search.kind_boost_mode = KindBoostMode::Always;
    // токен первого старта: при повторном старте каталога новый не выдаётся
    let tok = a.token.clone();
    a.st.shutdown().await;
    drop(a);
    // фоновые задачи открытия баз отпускают `Db` чуть позже (как `stop` в `tests/backup.rs`)
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let b = common::api::api_with(cfg).await;
    let r = b.tok(&tok, Method::POST, "/api/v1/search", Some(q)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(count(&j), 1, "порядок по contradicts: {j}");
}
