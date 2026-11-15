//! Явный фокус `/read` с моделью (M6 §6, задача 7b): откат на лексику по сроку
//! `read.focus_timeout_ms` и замер p95. Отдельный файл от `read_focus`: модель ставит счётчик
//! токенов процесса (`zam_store::set_token_counter`), и тесты без модели в том же процессе
//! считали бы токены её токенизатором (эталон `read_l2_m57.json` снят счётчиком bge-m3).
mod common;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use common::api::{Api, api};
use common::ml::{MINILM, external, have, ml, until, vector_ready};
use common::qdrant::TestQdrant;
use common::read::sections_note;
use serde_json::{Value, json};

const READ: &str = "/api/v1/read";
/// Предупреждение: фокус только по FTS-лексике листов.
const LEXICAL: &str = "focus lexical";

fn id(v: &Value) -> String {
    v["id"].as_str().unwrap().to_string()
}

async fn read_ok(a: &Api, body: Value) -> Value {
    let r = a.t(axum::http::Method::POST, READ, Some(body)).await;
    assert_eq!(r.status, axum::http::StatusCode::OK, "{}", r.text());
    r.json()
}

fn warnings(it: &Value) -> Vec<String> {
    it["warnings"]
        .as_array()
        .map(|w| w.iter().map(|s| s.as_str().unwrap().to_string()).collect())
        .unwrap_or_default()
}

/// Сервер с моделью `model` и внешним Qdrant; `None` — нет ORT/модели/Qdrant (вне CI).
async fn ml_api(
    name: &str,
    model: &str,
    tweak: impl FnOnce(&mut zam_server::config::ServerConfig),
) -> Option<(Api, TestQdrant)> {
    if !have(model) {
        return None;
    }
    let q = TestQdrant::start(name);
    let a = api(name, |c| {
        ml(c, model);
        external(c, &q);
        tweak(c);
    })
    .await;
    vector_ready(&a).await;
    Some((a, q))
}

/// Ждёт индексации узлов (эмбеддинги листов — в кэше `embeddings`).
async fn wait_indexed(a: &Api, ids: &[String]) {
    let d = a.st.users.get(a.st.admin_id).await.unwrap();
    until(
        "заметки проиндексированы",
        Duration::from_secs(240),
        async || {
            for n in ids {
                let node = zam_store::get_node(&d, n.parse().unwrap()).await.unwrap();
                if node.index_state != "indexed" {
                    return false;
                }
            }
            true
        },
    )
    .await;
}

/// С моделью фокус — по лексике и косинусу листов (без `focus lexical`); срок
/// `read.focus_timeout_ms` вышел (задержка точкой `test-hooks`) — только лексика с
/// `focus lexical`, и ответ не ждёт задержки; срок — один на вызов (5 целей с разными `focus`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn focus_timeout_lexical_fallback() {
    let Some((a, _q)) = ml_api("focus-timeout", MINILM, |c| {
        c.read.focus_timeout_ms = 2000;
        c.read.focus_timeout_max_ms = 2000;
    })
    .await
    else {
        return;
    };
    let n = id(&a
        .note("knowledge", "Восемь", &sections_note(8, 10, 5))
        .await);
    wait_indexed(&a, std::slice::from_ref(&n)).await;
    let body = json!({"items": [{"id": format!("n:{n}"), "focus": "детектор нейтрино"}],
                      "budget_tokens": 20000});
    let j = read_ok(&a, body.clone()).await;
    let it = &j["items"][0];
    assert_eq!(it["tier"], "focus", "{it}");
    assert!(warnings(it).is_empty(), "{it}");
    assert!(
        it["text"].as_str().unwrap().contains("детектор нейтрино"),
        "{it}"
    );
    a.st.focus_delay_ms.store(60_000, Ordering::SeqCst);
    let t = Instant::now();
    let j = read_ok(&a, body).await;
    let spent = t.elapsed();
    a.st.focus_delay_ms.store(0, Ordering::SeqCst);
    let it = &j["items"][0];
    assert_eq!(warnings(it), [LEXICAL], "{it}");
    assert!(
        it["text"].as_str().unwrap().contains("детектор нейтрино"),
        "{it}"
    );
    assert!(
        spent < Duration::from_secs(10),
        "ответ ждал задержку: {spent:?}"
    );
    // срок — один на вызов (ревью 7b, I-5): 5 целей с разными `focus` и задержкой эмбеддинга —
    // ответ не дольше срока (2 с) с допуском, у каждой цели — лексика
    let mut ids = vec![n];
    for i in 0..4 {
        let t = sections_note(8, 10, 2 + i);
        ids.push(id(&a.note("knowledge", &format!("Пять/{i}"), &t).await));
    }
    wait_indexed(&a, &ids).await;
    let items: Vec<Value> = ids
        .iter()
        .enumerate()
        .map(|(k, n)| json!({"id": format!("n:{n}"), "focus": format!("детектор нейтрино {k}")}))
        .collect();
    a.st.focus_delay_ms.store(60_000, Ordering::SeqCst);
    let t = Instant::now();
    let j = read_ok(&a, json!({"items": items, "budget_tokens": 20000})).await;
    let spent = t.elapsed();
    a.st.focus_delay_ms.store(0, Ordering::SeqCst);
    for it in j["items"].as_array().unwrap() {
        assert_eq!(warnings(it), [LEXICAL], "{it}");
    }
    assert!(
        spent < Duration::from_millis(2000 + 1500),
        "срок не общий на вызов: {spent:?}"
    );
}

/// Срок вызова — `focus_timeout_ms × K`, не больше потолка: 5 разных `focus` при умолчаниях
/// (80 мс × 5, потолок 300) — без откатов на лексику, ответ не дольше потолка и работы; после
/// ответа с отменой (срок вышел на задержке) следующий текст не эмбеддится — счётчик
/// `test-hooks` (повторное ревью 7b, пп. 1–2).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn focus_call_timeout_scales_and_cancel_stops() {
    let Some((a, _q)) = ml_api("focus-call-timeout", MINILM, |_| {}).await else {
        return;
    };
    let mut ids = Vec::new();
    for i in 0..5 {
        let t = sections_note(8, 10, 2 + i);
        ids.push(id(&a.note("knowledge", &format!("Срок/{i}"), &t).await));
    }
    wait_indexed(&a, &ids).await;
    let body = |tag: &str| {
        let items: Vec<Value> = ids
            .iter()
            .enumerate()
            .map(|(k, n)| json!({"id": format!("n:{n}"), "focus": format!("детектор нейтрино {tag}{k}")}))
            .collect();
        json!({"items": items, "budget_tokens": 20000})
    };
    // прогрев модели
    read_ok(&a, body("w")).await;
    let before = a.st.focus_embeds.load(Ordering::SeqCst);
    let t = Instant::now();
    let j = read_ok(&a, body("a")).await;
    let spent = t.elapsed();
    for it in j["items"].as_array().unwrap() {
        assert!(warnings(it).is_empty(), "откат на лексику: {it}");
    }
    assert!(spent < Duration::from_millis(300 + 3000), "{spent:?}");
    assert_eq!(a.st.focus_embeds.load(Ordering::SeqCst) - before, 5);
    // задержка перед каждым текстом дольше срока: ответ — лексика, после него задача видит
    // отмену и ни один текст не эмбеддит
    a.st.focus_delay_ms.store(1000, Ordering::SeqCst);
    let before = a.st.focus_embeds.load(Ordering::SeqCst);
    let j = read_ok(&a, body("b")).await;
    a.st.focus_delay_ms.store(0, Ordering::SeqCst);
    for it in j["items"].as_array().unwrap() {
        assert_eq!(warnings(it), [LEXICAL], "{it}");
    }
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(
        a.st.focus_embeds.load(Ordering::SeqCst),
        before,
        "после отмены эмбеддинг запущен"
    );
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    let i = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[i]
}

/// Замер p95 `/read` с фокусом на 5 целях (§6, §17 п. 8): CPU int8 ≤ 120 мс, GPU ≤ 50 мс.
/// Устройство — `ZAM_FOCUS_P95_DEVICE` (`cpu` по умолчанию, `cuda`): bge-m3-int8 на CPU-сборке ORT
/// или bge-m3-fp16 на GPU-сборке (`target/deps/<платформа>/onnxruntime-gpu/`). Модели — из
/// `ZAM_TEST_MODELS_DIR` (полные bge-m3). Запуск: `cargo test -p zam-server --test read_focus_ml
/// focus_p95 -- --ignored --nocapture`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "замер для журнала: bge-m3, CPU int8 или GPU fp16"]
async fn focus_p95() {
    let cuda = std::env::var("ZAM_FOCUS_P95_DEVICE").is_ok_and(|d| d == "cuda");
    let model = if cuda { "bge-m3-fp16" } else { "bge-m3-int8" };
    let Some((a, _q)) = ml_api(&format!("focus-p95-{model}"), model, |c| {
        if cuda {
            c.ml.device = zam_server::config::MlDevice::Cuda;
            c.ml.ort_dylib = Some(
                common::ml::repo()
                    .join("target")
                    .join("deps")
                    .join(common::ml::platform())
                    .join("onnxruntime-gpu")
                    .join(if cfg!(windows) {
                        "onnxruntime.dll"
                    } else {
                        "libonnxruntime.so"
                    }),
            );
        }
    })
    .await
    else {
        return;
    };
    let mut ids = Vec::new();
    for i in 0..5 {
        let t = sections_note(12, 25, 3 + i);
        ids.push(id(&a.note("knowledge", &format!("p95/{i}"), &t).await));
    }
    wait_indexed(&a, &ids).await;
    let queries = [
        "детектор нейтрино",
        "калибровка детектора",
        "журналы сервера",
        "порядок работы с конфигурацией",
        "раздел про нейтрино",
        "как настроить журналы",
        "конфигурация сервера",
        "калибровка",
        "детектор",
        "введение к руководству",
    ];
    let run = |same: bool, q: usize| {
        let items: Vec<Value> = ids
            .iter()
            .enumerate()
            .map(|(k, n)| {
                let f = if same {
                    format!("{} {q}", queries[q % queries.len()])
                } else {
                    format!("{} {q}-{k}", queries[(q + k) % queries.len()])
                };
                json!({"id": format!("n:{n}"), "focus": f})
            })
            .collect();
        json!({"items": items, "budget_tokens": 20000})
    };
    // прогрев
    for q in 0..5 {
        read_ok(&a, run(true, 1000 + q)).await;
    }
    eprintln!(
        "T7B-METRIC focus_p95 {model}: устройство {:?}, сборка {}",
        a.st.index.device,
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );
    // база: те же 5 целей без фокуса (L2 целиком) — доля самого `/read`
    let plain = json!({"items": ids.iter().map(|n| json!({"id": format!("n:{n}")})).collect::<Vec<_>>(),
                       "budget_tokens": 20000});
    let mut base = Vec::new();
    for _ in 0..30 {
        let t = Instant::now();
        read_ok(&a, plain.clone()).await;
        base.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    base.sort_by(f64::total_cmp);
    eprintln!(
        "T7B-METRIC focus_p95 {model}: без фокуса (L2, 5 целей): p50 {:.1} мс, p95 {:.1} мс",
        pct(&base, 0.5),
        pct(&base, 0.95)
    );
    // эмбеддинг запроса отдельно
    let emb = a.st.index.embedder.clone().unwrap();
    let mut em = Vec::new();
    for q in 0..30 {
        let t = Instant::now();
        emb.embed_query(&format!("{} {q}", queries[q % queries.len()]))
            .unwrap();
        em.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    em.sort_by(f64::total_cmp);
    eprintln!(
        "T7B-METRIC focus_p95 {model}: эмбеддинг запроса: p50 {:.1} мс, p95 {:.1} мс",
        pct(&em, 0.5),
        pct(&em, 0.95)
    );
    // (один `focus`), (разные, по одному эмбеддингу), (разные, пакетом)
    for (same, batch) in [(true, 2u8), (false, 2), (false, 1)] {
        a.st.focus_batch.store(batch, Ordering::SeqCst);
        let mut ms = Vec::new();
        let mut lexical = 0;
        for q in 0..60 {
            let t = Instant::now();
            let j = read_ok(&a, run(same, q + usize::from(batch) * 100)).await;
            ms.push(t.elapsed().as_secs_f64() * 1000.0);
            lexical += j["items"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|it| warnings(it).iter().any(|w| w == LEXICAL))
                .count();
        }
        ms.sort_by(f64::total_cmp);
        eprintln!(
            "T7B-METRIC focus_p95 {model} ({}), 5 целей, {} запросов, focus {}: p50 {:.1} мс, \
             p95 {:.1} мс, max {:.1} мс, откатов по сроку {lexical} из {}",
            if cuda { "GPU" } else { "CPU" },
            ms.len(),
            match (same, batch) {
                (true, _) => "один на запрос",
                (false, 1) => "свой у каждой цели, пакетом",
                _ => "свой у каждой цели, по одному",
            },
            pct(&ms, 0.5),
            pct(&ms, 0.95),
            ms[ms.len() - 1],
            ms.len() * ids.len()
        );
    }
    a.st.focus_batch.store(0, Ordering::SeqCst);
    // согласие топ-3 листов: пакет против одиночного эмбеддинга (повторное ревью 7b, п. 4)
    let model_id = a.st.index.model_id().unwrap().to_string();
    let d = a.st.users.get(a.st.admin_id).await.unwrap();
    let mut leaf_vecs: Vec<Vec<(String, Vec<f32>)>> = Vec::new();
    for n in &ids {
        let leaves = zam_store::node_leaves(&d, n.parse().unwrap())
            .await
            .unwrap();
        let keys: Vec<String> = leaves.iter().map(|l| l.embed_key.clone()).collect();
        let embs = zam_store::get_embeddings(&d, &model_id, &keys)
            .await
            .unwrap();
        leaf_vecs.push(
            leaves
                .iter()
                .filter_map(|l| Some((l.chunk_id.clone(), embs.get(&l.embed_key)?.dense.clone())))
                .collect(),
        );
    }
    let top3 = |q: &[f32], lv: &[(String, Vec<f32>)]| {
        let mut v: Vec<(f32, &str)> = lv
            .iter()
            .map(|(c, e)| (q.iter().zip(e).map(|(a, b)| a * b).sum::<f32>(), c.as_str()))
            .collect();
        v.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mut t: Vec<&str> = v.iter().take(3).map(|x| x.1).collect();
        t.sort_unstable();
        t.into_iter().map(String::from).collect::<Vec<_>>()
    };
    let emb = a.st.index.embedder.clone().unwrap();
    let (mut same_top, mut pairs, mut min_cos) = (0usize, 0usize, 1.0f32);
    for round in 0..10 {
        let texts: Vec<String> = (0..5)
            .map(|k| format!("{} {round}-{k} пакет", queries[(round + k) % queries.len()]))
            .collect();
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let batch = emb.embed_queries(&refs).unwrap();
        for (t, b) in texts.iter().zip(&batch) {
            let one = emb.embed_query(t).unwrap();
            let c: f32 = one.dense.iter().zip(&b.dense).map(|(x, y)| x * y).sum();
            min_cos = min_cos.min(c);
            for lv in &leaf_vecs {
                pairs += 1;
                same_top += usize::from(top3(&one.dense, lv) == top3(&b.dense, lv));
            }
        }
    }
    eprintln!(
        "T7B-METRIC focus_batch {model} ({}): согласие топ-3 листов пакет/одиночный {:.3} \
         ({same_top} из {pairs}), мин. косинус векторов {min_cos:.5}",
        if cuda { "GPU" } else { "CPU" },
        same_top as f64 / pairs as f64
    );
}
