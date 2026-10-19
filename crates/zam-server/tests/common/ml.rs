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
pub const READY: Duration = Duration::from_secs(90);

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

pub fn models() -> PathBuf {
    repo().join("target").join("models")
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

/// Ждёт включения векторного канала (коллекция создана и проверена).
pub async fn vector_ready(a: &Api) {
    let t = Instant::now();
    while a.st.index.vector_available().is_err() {
        assert!(
            t.elapsed() < READY,
            "векторный канал не включился: {:?}",
            a.st.index.vector_available()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Ждёт условия не дольше `limit`.
pub async fn until(what: &str, limit: Duration, f: impl AsyncFn() -> bool) {
    let t = Instant::now();
    while !f().await {
        assert!(t.elapsed() < limit, "не дождались за {limit:?}: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
