//! Модель и Qdrant для интеграционных тестов сервера: CPU-сборка ORT и модели из
//! `target/` (`cargo xtask fetch-deps && cargo xtask fetch-models --test`).
//!
//! Без ORT, модели или Qdrant тесты пропускаются; в CI (`ZAM_REQUIRE_ML=1`) — падают.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use zam_server::config::{MlDevice, QdrantMode, ServerConfig};

use super::api::Api;
use super::qdrant::{TestQdrant, qdrant_bin};

/// Тестовая модель (англоязычная MiniLM-L6, int8).
pub const MINILM: &str = "minilm-l6-int8";
/// Предел ожидания готовности коллекции (создание payload-индексов на HDD — секунды).
pub const READY: Duration = Duration::from_secs(240);

pub fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

pub fn platform() -> &'static str {
    if cfg!(windows) {
        "windows-x64"
    } else {
        "linux-x64"
    }
}

pub fn ort_cpu() -> PathBuf {
    let name = if cfg!(windows) {
        "onnxruntime.dll"
    } else {
        "libonnxruntime.so"
    };
    repo()
        .join("target")
        .join("deps")
        .join(platform())
        .join("onnxruntime")
        .join(name)
}

/// Каталог моделей тестов: `target/models`, или `ZAM_TEST_MODELS_DIR` — так локально
/// воспроизводится набор CI (`fetch-models --test`: тестовая модель и одиночный токенизатор
/// `tokenizers/bge-m3-int8/`) без удаления полных моделей.
pub fn models() -> PathBuf {
    std::env::var_os("ZAM_TEST_MODELS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo().join("target").join("models"))
}

/// Счётчик токенов bge-m3 (как у чанкера) из [`models`]: сначала одиночный токенизатор
/// `tokenizers/bge-m3-int8/` (его качает `cargo xtask fetch-models --test`, в том числе в CI),
/// затем установленные `bge-m3-int8`, `bge-m3-fp16`. Нет — `None`; с `ZAM_REQUIRE_ML` — падение.
pub fn bge_counter() -> Option<zam_ml::BgeTokenCounter> {
    let dir = models();
    let tokenizers = dir.join("tokenizers");
    for (d, id) in [
        (&tokenizers, "bge-m3-int8"),
        (&dir, "bge-m3-int8"),
        (&dir, "bge-m3-fp16"),
    ] {
        if let Ok(c) = zam_ml::BgeTokenCounter::load(d, id) {
            return Some(c);
        }
    }
    assert!(
        std::env::var_os("ZAM_REQUIRE_ML").is_none(),
        "нет токенизатора bge-m3 в {}: cargo xtask fetch-models --test",
        dir.display()
    );
    eprintln!("пропуск: нет токенизатора bge-m3 в {}", dir.display());
    None
}

/// ORT, модель и Qdrant на месте; иначе пропуск (в CI — падение).
pub fn have(model: &str) -> bool {
    let ok = ort_cpu().is_file()
        && models().join(model).join("model.toml").is_file()
        && qdrant_bin().is_file();
    if !ok {
        assert!(
            std::env::var_os("ZAM_REQUIRE_ML").is_none(),
            "нет ORT, модели {model} или Qdrant: cargo xtask fetch-deps && cargo xtask fetch-models --test"
        );
        eprintln!("пропуск: нет ORT, модели {model} или Qdrant");
    }
    ok
}

/// ML из `target/`: CPU-сборка ORT (одна на процесс тестов), модель `model`.
pub fn ml(c: &mut ServerConfig, model: &str) {
    c.ml.device = MlDevice::Cpu;
    c.ml.ort_dylib = Some(ort_cpu());
    c.ml.models_dir = Some(models());
    c.ml.embed_model = Some(model.into());
    c.ml.index_threads = 2;
    c.index.rollup_debounce_ms = 200;
}

/// Внешний Qdrant теста.
pub fn external(c: &mut ServerConfig, q: &TestQdrant) {
    c.qdrant.mode = QdrantMode::External;
    c.qdrant.url = Some(q.grpc_url());
    c.qdrant.api_key = Some(q.api_key.clone());
}

/// Состояние векторного канала для сообщения о таймауте: причина деградации, состояние
/// Qdrant (у `Broken` — причина) и снимок воркера (ошибка Qdrant, очередь, model_mismatch).
pub fn vector_diag(a: &Api) -> String {
    format!(
        "vector_available = {:?}; qdrant = {:?}; worker = {:?}",
        a.st.index.vector_available(),
        a.st.index.qdrant_state(),
        a.st.index.worker.status()
    )
}

/// Ждёт включения векторного канала (коллекция создана и проверена).
pub async fn vector_ready(a: &Api) {
    until_diag(
        "векторный канал не включился",
        READY,
        async || a.st.index.vector_available().is_ok(),
        async || vector_diag(a),
    )
    .await;
}

/// Ждёт условия не дольше `limit`.
pub async fn until(what: &str, limit: Duration, f: impl AsyncFn() -> bool) {
    until_diag(what, limit, f, async || String::new()).await;
}

/// [`until`] с диагностикой: при таймауте в сообщение идёт `diag()` (состояние на момент
/// отказа, а не только «не дождались»).
pub async fn until_diag(
    what: &str,
    limit: Duration,
    f: impl AsyncFn() -> bool,
    diag: impl AsyncFn() -> String,
) {
    let t = Instant::now();
    while !f().await {
        if t.elapsed() >= limit {
            panic!("не дождались за {limit:?}: {what}; {}", diag().await);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
