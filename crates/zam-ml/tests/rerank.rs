//! Реранкер (задача 9 M2): описатель, отказ без модели и прогон на скачанных моделях.
//!
//! `rerank_smoke` — на настоящих весах (`cargo xtask fetch-models --id bge-reranker-v2-m3`,
//! для jina — ещё `--id jina-reranker-v2 --accept-nc-license`): релевантная пара выше
//! нерелевантной и время реранка 20 × 256 токенов. Запуск:
//! `cargo test --release -p zam-ml --test rerank -- --ignored --nocapture`.

mod common;

use std::time::Instant;

use common::*;
use zam_ml::{Device, DevicePref, MlError, Reranker};

#[test]
fn missing_reranker_is_no_model() {
    let dir = tmp_dir("rerank-missing");
    let e = Reranker::load(&config(ort_cpu(), dir, DevicePref::Cpu), "jina-reranker-v2")
        .err()
        .expect("модели нет");
    assert!(matches!(e, MlError::NoModel(_)), "{e}");
    assert!(e.to_string().contains("models fetch"), "{e}");
}

#[test]
fn embedder_is_not_a_reranker() {
    if !have(&ort_cpu(), "minilm-l6-int8") {
        return;
    }
    let e = Reranker::load(
        &config(ort_cpu(), models_dir(), DevicePref::Cpu),
        "minilm-l6-int8",
    )
    .err()
    .expect("эмбеддер не реранкер");
    assert!(matches!(e, MlError::Spec(_)), "{e}");
}

/// Есть ли реранкер в `target/models` (без паники в CI: модели реранкера туда не качаются).
fn have_reranker(id: &str) -> bool {
    let ok = ort_cpu().is_file() && models_dir().join(id).join("model.toml").is_file();
    if !ok {
        eprintln!("пропуск {id}: cargo xtask fetch-models --id {id}");
    }
    ok
}

/// Запрос и 20 фрагментов по ~256 токенов: первый релевантен, остальные — нет.
fn corpus() -> (String, Vec<String>) {
    let query = "Как настроить окончания строк LF для bash-скриптов в Windows?".to_string();
    let relevant = "Скрипты bash в Windows нужно сохранять с окончаниями строк LF: с CRLF \
                    интерпретатор видит лишний символ \\r и падает. В git задайте \
                    core.autocrlf=false и добавьте в .gitattributes правило *.sh text eol=lf. ";
    let noise = [
        "Тесто для хлеба на закваске замешивают с вечера и выпекают в очень горячей духовке. ",
        "Marathon training means long runs every weekend and a slow increase in weekly mileage. ",
        "Кошкам нужна белковая пища, свежая вода и минимум углеводов в рационе. ",
        "Замена масла в двигателе: слить старое, заменить фильтр, залить новое. ",
    ];
    let mut passages = Vec::new();
    for i in 0..20 {
        let base = if i == 0 {
            relevant
        } else {
            noise[i % noise.len()]
        };
        let mut s = String::new();
        // ~256 токенов: повтор базового текста
        while s.chars().count() < 1100 {
            s.push_str(base);
        }
        passages.push(s);
    }
    (query, passages)
}

fn smoke(id: &str, ort: std::path::PathBuf, pref: DevicePref) {
    let t = Instant::now();
    let mut cfg = config(ort, models_dir(), pref);
    // как в сервере по умолчанию: ml.index_threads = 0 — все физические ядра
    cfg.index_threads = 0;
    let r = Reranker::load(&cfg, id).expect("загрузка реранкера");
    let load = t.elapsed();
    let (q, ps) = corpus();
    let refs: Vec<&str> = ps.iter().map(String::as_str).collect();
    // короткая проверка: релевантная пара выше нерелевантной
    let s = r
        .score(
            "how to cook pasta",
            &[
                "Boil water, add salt and cook the pasta for ten minutes.",
                "The stock market fell sharply on Monday.",
            ],
        )
        .unwrap();
    assert!(s[0] > s[1], "{id}: {s:?}");
    assert!(s.iter().all(|x| (0.0..=1.0).contains(x)));
    // прогрев и замер 20 × 256
    r.score_capped(&q, &refs, 256).unwrap();
    let t = Instant::now();
    let runs = 3;
    let mut scores = Vec::new();
    for _ in 0..runs {
        scores = r.score_capped(&q, &refs, 256).unwrap();
    }
    let dt = t.elapsed() / runs;
    assert_eq!(scores.len(), 20);
    let best = scores
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .unwrap()
        .0;
    eprintln!(
        "{id} на {} ({}): загрузка {load:?}; реранк 20×256 — {dt:?}; релевантный {:.3}, \
         лучший нерелевантный {:.3}",
        r.device().device,
        r.device().reason,
        scores[0],
        scores[1..].iter().copied().fold(f32::MIN, f32::max)
    );
    assert_eq!(best, 0, "{id}: {scores:?}");
}

#[test]
#[ignore = "нужны веса реранкера (cargo xtask fetch-models --id bge-reranker-v2-m3; jina — с --accept-nc-license)"]
fn rerank_smoke() {
    let mut any = false;
    // CPU: jina (int8, вариант для CPU) и bge (fp16 на CPU — для сравнения)
    for id in ["jina-reranker-v2", "bge-reranker-v2-m3"] {
        if have_reranker(id) {
            smoke(id, ort_cpu(), DevicePref::Cpu);
            any = true;
        }
    }
    assert!(any, "нет ни одного реранкера в target/models");
}

/// bge-reranker-v2-m3 fp16 на CUDA (GPU-сборка ORT и библиотеки NVIDIA в `<ORT>/cuda/`).
/// В одном процессе ORT грузится один раз, поэтому — отдельным запуском:
/// `cargo test --release -p zam-ml --test rerank rerank_smoke_cuda -- --ignored --nocapture`.
#[test]
#[ignore = "нужны GPU, CUDA (zam-server cuda fetch) и bge-reranker-v2-m3"]
fn rerank_smoke_cuda() {
    const ID: &str = "bge-reranker-v2-m3";
    if !ort_gpu().is_file() || !models_dir().join(ID).join("model.toml").is_file() {
        eprintln!("пропуск: нет GPU-сборки ORT или {ID}");
        return;
    }
    let r = Reranker::load(&config(ort_gpu(), models_dir(), DevicePref::Cuda), ID).unwrap();
    assert!(
        matches!(r.device().device, Device::Cuda { .. }),
        "CUDA недоступна: {}",
        r.device().reason
    );
    drop(r);
    smoke(ID, ort_gpu(), DevicePref::Cuda);
}
