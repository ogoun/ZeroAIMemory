//! Гибридный `POST /search` (задача 8 M2): MiniLM на CPU-сборке ORT, настоящий Qdrant
//! 1.19.2, английский корпус из 20 заметок. Без ORT или MiniLM тесты пропускаются;
//! в CI (`ZAM_REQUIRE_ML=1`) — падают.

mod common;

use std::time::{Duration, Instant};

use axum::http::{Method, StatusCode};
use common::api::{Api, Resp, api};
use common::ml::{MINILM, external, have, ml, until, vector_ready};
use common::qdrant::TestQdrant;
use serde_json::{Value, json};
use zam_core::Id;
use zam_index::VectorFilter;

/// Перефраз заметки `kitchen/sourdough` без общих слов с корпусом.
const PARAPHRASE: &str = "naturally fermented loaf";

/// Пианино: секция «Daily routine» из трёх длинных абзацев с маркерами.
fn piano() -> String {
    let filler = |w: &str| {
        (0..90)
            .map(|i| format!("{w} scales arpeggios tempo{i}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    format!(
        "# Piano practice\n\nKeep a steady schedule at the keyboard.\n\n## Daily routine\n\n\
         ALPHAMARK opens the warmup. {}\n\nBETAMARK sits in the middle. {}\n\n\
         GAMMAMARK closes the session. {}\n\n## Repertoire\n\nPick one sonata per month.\n",
        filler("first"),
        filler("second"),
        filler("third")
    )
}

/// 20 заметок на разные темы; слова [`PARAPHRASE`] не встречаются нигде.
fn corpus() -> Vec<(&'static str, String)> {
    let plain = [
        (
            "kitchen/sourdough",
            "# Sourdough bread\n\nMix flour, water and a starter culture, knead the dough, \
             let it rise overnight and bake it in a very hot oven until the crust turns brown.\n",
        ),
        (
            "garden/tomatoes",
            "# Tomato plants\n\nPlant seedlings in full sun, water the soil deeply, tie the stems \
             to stakes and pick the ripe red tomatoes in late summer.\n",
        ),
        (
            "sport/marathon",
            "# Marathon training\n\nRun long distances every weekend, increase weekly mileage \
             slowly and taper two weeks before the race.\n",
        ),
        (
            "dev/rust-ownership",
            "# Rust ownership\n\nThe borrow checker rejects two mutable borrows; error E0502 \
             appears when a value is borrowed as mutable and immutable. Use std::mem::take to \
             move a value out of a mutable reference.\n",
        ),
        (
            "dev/git-rebase",
            "# Git rebase\n\nRewrite local commits onto the main branch, resolve conflicts \
             and continue; never rewrite shared history.\n",
        ),
        (
            "money/taxes",
            "# Tax deadlines\n\nFile the annual income return before April and keep receipts \
             for deductions.\n",
        ),
        (
            "pets/cat-food",
            "# Cat nutrition\n\nAdult cats need protein rich meals, fresh water and few \
             carbohydrates.\n",
        ),
        (
            "car/oil",
            "# Engine oil change\n\nDrain the old oil, replace the filter and refill the \
             engine every ten thousand kilometers.\n",
        ),
        (
            "kitchen/coffee",
            "# Coffee brewing\n\nGrind beans right before brewing, use water just below boiling \
             and steep for four minutes in a French press.\n",
        ),
        (
            "games/chess",
            "# Chess openings\n\nControl the center early, develop knights before bishops and \
             castle the king.\n",
        ),
        (
            "music/guitar",
            "# Guitar tuning\n\nStandard tuning is E A D G B E; tune each string against a \
             reference pitch.\n",
        ),
        (
            "home/insulation",
            "# Attic insulation\n\nLay mineral wool between the joists to reduce heat loss in \
             winter.\n",
        ),
        (
            "sport/bicycle",
            "# Bicycle repair\n\nPatch a punctured inner tube, true the wheel and lubricate the \
             chain.\n",
        ),
        (
            "art/photography",
            "# Photo exposure\n\nBalance aperture, shutter speed and ISO to expose the scene \
             correctly.\n",
        ),
        (
            "craft/knitting",
            "# Knitting basics\n\nCast on stitches, alternate knit and purl rows and bind off at \
             the end.\n",
        ),
        (
            "farm/bees",
            "# Beekeeping\n\nInspect the hive for the queen, add supers in spring and harvest \
             honey in autumn.\n",
        ),
        (
            "travel/camping",
            "# Camping tents\n\nPitch the tent on flat ground, stake the corners and keep the \
             rain fly taut.\n",
        ),
        (
            "dev/python-venv",
            "# Python virtual environments\n\nCreate an isolated environment per project and \
             pin dependency versions.\n",
        ),
        (
            "health/sleep",
            "# Sleep hygiene\n\nGo to bed at the same hour, avoid screens late at night and keep \
             the bedroom dark.\n",
        ),
    ];
    let mut out: Vec<(&str, String)> = plain.iter().map(|(p, b)| (*p, b.to_string())).collect();
    out.push(("music/piano", piano()));
    out
}

async fn search(a: &Api, body: Value) -> Resp {
    a.t(Method::POST, "/api/v1/search", Some(body)).await
}

async fn search_adm(a: &Api, body: Value) -> Value {
    let r = a
        .adm(Method::POST, "/api/v1/search", Some(body), None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

fn paths(j: &Value) -> Vec<String> {
    j["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["path"].as_str().unwrap().to_string())
        .collect()
}

async fn state(a: &Api, node: &str) -> String {
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    zam_store::get_node(&db, node.parse().unwrap())
        .await
        .unwrap()
        .index_state
}

/// Сервер с MiniLM и внешним Qdrant, корпус записан и проиндексирован.
/// Возвращает `(Api, Qdrant, id заметок по пути)`.
async fn indexed(name: &str) -> Option<(Api, TestQdrant, Vec<(String, String)>)> {
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
    let mut ids = Vec::new();
    for (path, body) in corpus() {
        let n = a.note("knowledge", path, &body).await;
        ids.push((path.to_string(), n["id"].as_str().unwrap().to_string()));
    }
    until(
        "корпус проиндексирован",
        Duration::from_secs(240),
        async || {
            for (_, id) in &ids {
                if state(&a, id).await != "indexed" {
                    return false;
                }
            }
            true
        },
    )
    .await;
    Some((a, q, ids))
}

fn id_of<'a>(ids: &'a [(String, String)], path: &str) -> &'a str {
    &ids.iter().find(|(p, _)| p == path).unwrap().1
}

/// Точек узла в Qdrant (все уровни заметки).
async fn node_points(a: &Api, prefix: &str) -> u64 {
    let f = VectorFilter {
        user_id: a.st.admin_id,
        space_ids: vec![],
        path_prefix: Some(prefix.into()),
        levels: ["chunk", "section", "note"].map(String::from).to_vec(),
    };
    a.st.index.vector.as_ref().unwrap().count(&f).await.unwrap()
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    let i = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[i]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hybrid_paraphrase_identifier_expand_budget() {
    let Some((a, _q, _ids)) = indexed("hyb-main").await else {
        return;
    };

    // перефраз без общих слов: только векторный канал, заметка в top-3
    let r = search(&a, json!({"query": PARAPHRASE, "top_k": 5})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["degraded"], false, "{j}");
    assert!(j["degraded_reason"].is_null(), "{j}");
    // индекс не пересобирается — предупреждения index rebuilding нет
    assert_eq!(j["warnings"], json!([]), "{j}");
    for t in ["fts_ms", "embed_ms", "vector_ms", "fuse_ms"] {
        assert!(j["timings"][t].is_number(), "{t}: {j}");
    }
    let top3: Vec<String> = paths(&j).into_iter().take(3).collect();
    assert!(top3.contains(&"kitchen/sourdough".to_string()), "{j}");
    let it = j["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["path"] == "kitchen/sourdough")
        .unwrap();
    assert_eq!(it["sources"], json!(["vector"]), "{it}");
    // сводка заменена листом (§9.2 п. 6)
    assert_eq!(it["level"], "chunk", "{it}");
    // контроль: FTS сам этот запрос не находит
    let fts_only = {
        let db = a.st.users.get(a.st.admin_id).await.unwrap();
        let k = zam_store::list_spaces(&db)
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.kind == zam_store::SpaceKind::Knowledge)
            .unwrap()
            .id;
        zam_store::search_fts(
            &db,
            zam_store::FtsQuery {
                text: PARAPHRASE.into(),
                space_ids: vec![k],
                path_prefix: None,
                top_k: 10,
            },
        )
        .await
        .unwrap()
    };
    assert!(fts_only.is_empty(), "{fts_only:?}");

    // точный идентификатор — через FTS (bm25 и trigram), первым
    let j = search(&a, json!({"query": "E0502"})).await.json();
    assert_eq!(paths(&j)[0], "dev/rust-ownership", "{j}");
    let src = j["items"][0]["sources"].as_array().unwrap().clone();
    assert!(
        src.contains(&json!("fts")) && src.contains(&json!("trigram")),
        "{j}"
    );
    let j = search(&a, json!({"query": "std::mem::take", "format": "md"})).await;
    let t = j.text();
    assert!(
        t.starts_with("1. n:") && t.contains("Knowledge/dev/rust-ownership"),
        "{t}"
    );
    assert!(!t.contains("degraded"), "{t}");

    // expand=section — весь текст секции, а не один лист
    let plain = search(
        &a,
        json!({"query": "BETAMARK", "mode": "chunks", "top_k": 1, "budget_tokens": 5000}),
    )
    .await
    .json();
    let text = plain["items"][0]["text"].as_str().unwrap();
    assert!(text.contains("BETAMARK"), "{plain}");
    assert!(
        !(text.contains("ALPHAMARK") && text.contains("GAMMAMARK")),
        "лист должен быть частью секции: {text}"
    );
    let j = search(
        &a,
        json!({"query": "BETAMARK", "mode": "chunks", "top_k": 1, "expand": "section",
               "budget_tokens": 5000}),
    )
    .await
    .json();
    let sec = j["items"][0]["text"].as_str().unwrap();
    assert!(sec.starts_with("## Daily routine"), "{sec}");
    for m in ["ALPHAMARK", "BETAMARK", "GAMMAMARK"] {
        assert!(sec.contains(m), "{m}: {sec}");
    }
    assert!(!sec.contains("Repertoire"), "{sec}");
    let j = search(
        &a,
        json!({"query": "BETAMARK", "mode": "chunks", "top_k": 1, "expand": "neighbors",
               "budget_tokens": 5000}),
    )
    .await
    .json();
    let nb = j["items"][0]["text"].as_str().unwrap();
    assert!(nb.contains("BETAMARK") && nb.len() > text.len(), "{nb}");

    // budget_tokens = 200 обрезает, но хотя бы один элемент
    let big = search(
        &a,
        json!({"query": "scales arpeggios", "mode": "chunks", "budget_tokens": 5000}),
    )
    .await
    .json();
    let small = search(
        &a,
        json!({"query": "scales arpeggios", "mode": "chunks", "budget_tokens": 200}),
    )
    .await
    .json();
    // токены — тем же счётчиком, что у сервера (токенизатор MiniLM в этом процессе)
    let tokens = |j: &Value| -> usize {
        j["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| {
                let n = zam_store::count_tokens(i["text"].as_str().unwrap());
                // `tokens` — размер отданного текста
                assert_eq!(i["tokens"], n, "{i}");
                n
            })
            .sum()
    };
    let items = small["items"].as_array().unwrap();
    assert!(!items.is_empty(), "{small}");
    assert!(tokens(&small) < tokens(&big), "{small}");
    assert!(tokens(&small) <= 200, "{small}");
    assert_eq!(items.last().unwrap()["text_truncated"], true, "{small}");

    // повторная секция в expand=section сливается, выдача добирается до top_k
    let j = search(
        &a,
        json!({"query": "scales arpeggios", "mode": "chunks", "expand": "section",
               "top_k": 3, "budget_tokens": 20000}),
    )
    .await
    .json();
    let items = j["items"].as_array().unwrap();
    assert_eq!(items.len(), 3, "{j}");
    let piano: Vec<&Value> = items
        .iter()
        .filter(|i| i["path"] == "music/piano")
        .collect();
    assert_eq!(piano.len(), 1, "{j}");
    assert!(piano[0]["chunk_ids"].as_array().unwrap().len() >= 2, "{j}");

    // замер: p50/p95 гибридного /search (brief, разные запросы — мимо LRU запросов)
    let queries = [
        "how to keep a sourdough starter alive",
        "watering plants in hot weather",
        "long distance running plan",
        "borrow checker mutable reference",
        "squash commits before merge",
    ];
    let mut lat = Vec::new();
    for i in 0..60 {
        let q = format!("{} {i}", queries[i % queries.len()]);
        let t = Instant::now();
        let r = search(&a, json!({"query": q})).await;
        lat.push(t.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(r.status, StatusCode::OK);
        assert_eq!(r.json()["degraded"], false);
    }
    lat.sort_by(f64::total_cmp);
    eprintln!(
        "T8-METRIC /search гибрид (MiniLM, 20 заметок, {} запросов): p50 {:.1} мс, p95 {:.1} мс, max {:.1} мс",
        lat.len(),
        pct(&lat, 0.5),
        pct(&lat, 0.95),
        lat[lat.len() - 1]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn human_deleted_and_excluded_not_in_vector_channel() {
    let Some((a, _q, ids)) = indexed("hyb-filter").await else {
        return;
    };
    let r = a
        .adm(
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": "human", "path": "family/recipe", "kind": "note",
                        "content": "# Grandmother recipe\n\nMix flour, water and a starter \
                                    culture, let the dough rise overnight and bake it in a hot oven.\n"})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let human = r.json()["id"].as_str().unwrap().to_string();

    // Human по токену не виден никак
    for body in [
        json!({"query": PARAPHRASE, "scope": {"include_human": true}}),
        json!({"query": "Grandmother", "scope": {"spaces": ["human"], "include_human": true}}),
    ] {
        let j = search(&a, body).await.json();
        assert!(!paths(&j).contains(&"family/recipe".to_string()), "{j}");
    }
    // администратор: в векторном канале Human нет — перефраз его не находит,
    // точное слово находит только FTS
    let j = search_adm(
        &a,
        json!({"query": PARAPHRASE, "scope": {"spaces": ["human"], "include_human": true}}),
    )
    .await;
    assert!(j["items"].as_array().unwrap().is_empty(), "{j}");
    assert_eq!(j["degraded"], false, "{j}");
    let j = search_adm(
        &a,
        json!({"query": PARAPHRASE, "scope": {"include_human": true}, "top_k": 20}),
    )
    .await;
    assert!(paths(&j).contains(&"kitchen/sourdough".to_string()), "{j}");
    assert!(!paths(&j).contains(&"family/recipe".to_string()), "{j}");
    let j = search_adm(
        &a,
        json!({"query": "Grandmother", "scope": {"include_human": true}}),
    )
    .await;
    // FTS-хит ранга 1 делит первое место с векторным хитом ранга 1 (равные веса RRF,
    // ничья решается по chunk_id), поэтому позиция не проверяется — только источник
    let it = j["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["path"] == "family/recipe")
        .unwrap_or_else(|| panic!("{j}"));
    assert_eq!(it["sources"], json!(["fts"]), "{j}");
    assert_eq!(it["node_id"], human.as_str());
    // смешанная область (Knowledge + Human), ослабленный запрос: ни в одном чанке нет обоих
    // слов. Human-заметку векторный канал не видит, поэтому её bm25 не понижается
    // `w_fts_relaxed` (0,25) и она не тонет под ~20 векторными кандидатами Knowledge
    let j = search_adm(
        &a,
        json!({"query": "Grandmother zeppelin", "scope": {"include_human": true}}),
    )
    .await;
    assert_eq!(j["degraded"], false, "{j}");
    assert!(paths(&j).contains(&"family/recipe".to_string()), "{j}");

    // узел удалён (и другой исключён) в SQLite без delete в Qdrant: точки остались,
    // но векторный хит в выдачу не попадает
    let tomato_q = "growing crimson backyard vines";
    let run_q = "jogging competition preparation";
    for (q, path) in [(tomato_q, "garden/tomatoes"), (run_q, "sport/marathon")] {
        // контроль: до порчи узел находится, и только векторным каналом
        let j = search(&a, json!({"query": q, "top_k": 5})).await.json();
        let it = j["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["path"] == path);
        assert_eq!(
            it.map(|i| i["sources"].clone()),
            Some(json!(["vector"])),
            "контроль {path}: {j}"
        );
    }
    let tomato: Id = id_of(&ids, "garden/tomatoes").parse().unwrap();
    let marathon: Id = id_of(&ids, "sport/marathon").parse().unwrap();
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    db.write(move |tx| {
        tx.execute(
            "UPDATE nodes SET deleted_at = '2026-10-19T00:00:00.000000Z' WHERE id = ?1",
            [tomato],
        )?;
        tx.execute(
            "UPDATE nodes SET index_state = 'excluded' WHERE id = ?1",
            [marathon],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    assert!(node_points(&a, "garden/tomatoes").await > 0);
    assert!(node_points(&a, "sport/marathon").await > 0);
    for (q, path) in [(tomato_q, "garden/tomatoes"), (run_q, "sport/marathon")] {
        let j = search(&a, json!({"query": q, "top_k": 20})).await.json();
        assert!(!paths(&j).contains(&path.to_string()), "{path}: {j}");
        assert_eq!(j["degraded"], false, "{j}");
    }
}

/// Узел Knowledge, который векторный канал не видит (`index_state` `pending` — в очереди,
/// `failed` — индексация не удалась, точек нет): ослабленный bm25 для него не понижается
/// `w_fts_relaxed`, иначе находка только по слову заметки тонет под векторными кандидатами.
/// Контроль — тот же узел без точек, но `indexed`: вес понижен, заметка вне top-10.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pending_and_failed_notes_keep_relaxed_fts_weight() {
    let Some((a, _q, ids)) = indexed("hyb-blind").await else {
        return;
    };
    let cat: Id = id_of(&ids, "pets/cat-food").parse().unwrap();
    // точки узла удалены (как у заметки, ещё не проиндексированной или с неудачей)
    a.st.index
        .vector
        .as_ref()
        .unwrap()
        .delete_nodes(a.st.admin_id, &[cat])
        .await
        .unwrap();
    assert_eq!(node_points(&a, "pets/cat-food").await, 0);
    let db = a.st.users.get(a.st.admin_id).await.unwrap();
    // «zeppelin» нет ни в одном чанке: ослабленный запрос, bm25 находит только cat-food
    let q = json!({"query": "carbohydrates zeppelin", "top_k": 10});
    for st in ["indexed", "pending", "failed"] {
        let sql = format!("UPDATE nodes SET index_state = '{st}' WHERE id = ?1");
        db.write(move |tx| {
            tx.execute(&sql, [cat])?;
            Ok(())
        })
        .await
        .unwrap();
        let j = search(&a, q.clone()).await.json();
        assert_eq!(j["degraded"], false, "{st}: {j}");
        let found = paths(&j).contains(&"pets/cat-food".to_string());
        assert_eq!(found, st != "indexed", "{st}: {j}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn qdrant_down_degrades_to_fts() {
    let Some((a, mut q, _ids)) = indexed("hyb-down").await else {
        return;
    };
    q.stop();
    // и сбой запроса, и выключенный воркером канал дают одну причину
    for _ in 0..2 {
        let r = search(&a, json!({"query": "sourdough bread"})).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        let j = r.json();
        assert_eq!(j["degraded"], true, "{j}");
        assert_eq!(j["degraded_reason"], "qdrant_down", "{j}");
        assert_eq!(paths(&j)[0], "kitchen/sourdough", "{j}");
        assert_eq!(j["items"][0]["sources"], json!(["fts"]), "{j}");
        until(
            "канал выключен воркером",
            Duration::from_secs(30),
            async || a.st.index.vector_available().is_err(),
        )
        .await;
    }
    // FTS-only: вес ослабленного bm25 не меняется. «sourdough 0502» — ни в одном чанке нет
    // обоих слов (ослабленный запрос): bm25 находит только хлеб, trigram по «0502» — только
    // заметку о Rust (`E0502`). С `w_fts = 1.0` первым идёт bm25, с 0,25 первой была бы
    // trigram-находка (вес 0,7)
    let j = search(&a, json!({"query": "sourdough 0502"})).await.json();
    assert_eq!(j["degraded"], true, "{j}");
    assert_eq!(
        paths(&j)[..2],
        ["kitchen/sourdough", "dev/rust-ownership"],
        "{j}"
    );
    assert_eq!(j["items"][1]["sources"], json!(["trigram"]), "{j}");
    let t = search(&a, json!({"query": "sourdough bread", "format": "md"}))
        .await
        .text();
    assert!(
        t.trim_end()
            .ends_with("> degraded: qdrant_down — векторная база Qdrant запускается или недоступна (только FTS)"),
        "{t}"
    );
}

/// `rerank=on` с настоящим реранкером поверх выдачи FTS (эмбеддера нет — без Qdrant):
/// релевантная заметка поднимается наверх, у переставленных — `rerank_score`. Реранкеры в CI
/// не качаются: без весов в `target/models` тест пропускается
/// (`cargo xtask fetch-models --id bge-reranker-v2-m3`; jina — с `--accept-nc-license`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rerank_on_reorders_fts_results() {
    let models = common::ml::models();
    let Some(id) = ["jina-reranker-v2", "bge-reranker-v2-m3"]
        .into_iter()
        .find(|id| models.join(id).join("model.toml").is_file())
    else {
        eprintln!("пропуск: нет весов реранкера в target/models");
        return;
    };
    if !common::ml::ort_cpu().is_file() {
        eprintln!("пропуск: нет ORT");
        return;
    }
    let a = api("rerank-fts", |c| {
        // эмбеддера нет (FTS-only), ORT и каталог моделей — из target/
        ml(c, "no-such-embedder");
        c.ml.rerank_model_cpu = id.into();
        c.ml.index_threads = 0;
    })
    .await;
    for (path, body) in [
        (
            "money/bread-tax",
            "# Bread tax\n\nThe bread tax, bread prices and bread subsidies: bread, bread, bread \
             in the city budget debate.\n",
        ),
        (
            "kitchen/bread",
            "# Baking bread at home\n\nMix flour, water, salt and yeast, knead the dough, let it \
             rise for two hours and bake the bread in a hot oven for forty minutes.\n",
        ),
        (
            "history/bread-riots",
            "# Bread riots\n\nIn 1789 bread shortages led to bread riots in Paris.\n",
        ),
    ] {
        a.note("knowledge", path, body).await;
    }
    let q = "how do I bake bread at home in the oven";
    let base = search(&a, json!({"query": q})).await.json();
    assert_eq!(base["warnings"], json!([]), "{base}");
    let t = Instant::now();
    let r = search(&a, json!({"query": q, "rerank": "on"})).await;
    let first = t.elapsed();
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["warnings"], json!([]), "{j}");
    assert!(j["timings"]["rerank_ms"].is_number(), "{j}");
    let items = j["items"].as_array().unwrap();
    assert_eq!(items[0]["path"], "kitchen/bread", "{j}");
    let scores: Vec<f64> = items
        .iter()
        .map(|i| i["rerank_score"].as_f64().expect("rerank_score"))
        .collect();
    assert!(scores.windows(2).all(|w| w[0] >= w[1]), "{scores:?}");
    // второй запрос — реранкер уже в памяти
    let t = Instant::now();
    let j2 = search(&a, json!({"query": q, "rerank": "on", "format": "md"})).await;
    eprintln!(
        "rerank=on ({id}): первый запрос {first:?} (с загрузкой), второй {:?}",
        t.elapsed()
    );
    assert!(j2.text().starts_with("1. "), "{}", j2.text());
    assert!(!j2.text().contains("> warning"), "{}", j2.text());
}
