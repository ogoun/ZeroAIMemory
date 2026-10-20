//! Загрузка модели эмбеддингов при старте сервера (§8.4) и адаптер к воркеру индексации.
//!
//! Порядок: `init_runtime` (ONNX Runtime по `ml.ort_dylib`) → `Embedder::load`. Ошибки не
//! валят сервер: он работает в режиме FTS-only, причина пишется в лог одной строкой WARN
//! с подсказкой и попадает в `degraded_reason` поиска и `/index/status`.
//!
//! Модель по умолчанию: `bge-m3-fp16` в GPU-комплекте (рядом с ORT лежит CUDA EP), если
//! `ml.device` не `cpu` и CUDA действительно поднялась; иначе `bge-m3-int8`. GPU-комплект
//! без GPU откатывается на int8 (fp16 на CPU медленнее и дал бы другой `model_id`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use zam_index::PassageEmbedder;
use zam_ml::{Device, DeviceReport, Embedder, MlConfig, MlError};

use crate::config::{MlDevice, ServerConfig};
use crate::index_state::DegradedReason;

/// Модель CPU-комплекта (int8, только CPU).
pub const MODEL_CPU: &str = "bge-m3-int8";
/// Модель для CUDA (fp16).
pub const MODEL_GPU: &str = "bge-m3-fp16";

/// Итог загрузки ML.
pub struct MlLoad {
    pub embedder: Option<Arc<Embedder>>,
    pub device: Option<DeviceReport>,
    /// Причина режима FTS-only и текст для лога/статуса.
    pub error: Option<(DegradedReason, String)>,
}

impl std::fmt::Debug for MlLoad {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MlLoad")
            .field("model", &self.embedder.as_ref().map(|e| e.model_id()))
            .field("device", &self.device)
            .field("error", &self.error)
            .finish()
    }
}

impl MlLoad {
    /// Модель не загружена: причина пишется в лог одной строкой WARN с подсказкой.
    pub fn failed(reason: DegradedReason, msg: String) -> MlLoad {
        tracing::warn!(
            reason = reason.as_str(),
            error = %msg,
            hint = reason.hint(),
            "векторный поиск выключен (FTS-only)"
        );
        MlLoad {
            embedder: None,
            device: None,
            error: Some((reason, msg)),
        }
    }
}

/// Точный счётчик токенов модели без ONNX Runtime (только токенизатор): для
/// `zam-server migrate`, где модель не нужна, а перечанковка R1 должна идти тем же
/// счётчиком, что и сервер. Модели bge-m3 int8/fp16 делят один токенизатор.
pub fn token_counter(cfg: &ServerConfig) -> Option<zam_ml::BgeTokenCounter> {
    let dir = cfg.ml.models_dir(&cfg.data_dir);
    let ids: Vec<&str> = match cfg
        .ml
        .embed_model
        .as_deref()
        .filter(|m| !m.trim().is_empty())
    {
        Some(m) => vec![m],
        None => vec![MODEL_CPU, MODEL_GPU],
    };
    ids.into_iter()
        .find_map(|id| zam_ml::BgeTokenCounter::load(&dir, id).ok())
}

/// Настройки `zam-ml` из `[ml]`.
pub fn ml_config(cfg: &ServerConfig) -> anyhow::Result<MlConfig> {
    let ml = &cfg.ml;
    Ok(MlConfig {
        device: ml.device.into(),
        models_dir: ml.models_dir(&cfg.data_dir),
        ort_dylib: ml
            .ort_dylib
            .clone()
            .unwrap_or_else(MlConfig::default_ort_dylib),
        index_threads: ml.index_threads,
        query_threads: ml.query_threads,
        rerank_sessions: ml.rerank_sessions,
        unload_after: ml.unload_after()?,
    })
}

/// Абсолютный путь к ORT (относительный — от каталога программы, как в `zam-ml`).
pub(crate) fn ort_path(p: &Path) -> PathBuf {
    if p.is_absolute() {
        return p.to_path_buf();
    }
    crate::qdrant::exe_dir()
        .map(|d| d.join(p))
        .unwrap_or_else(|_| p.to_path_buf())
}

/// GPU-комплект: рядом с ORT лежит CUDA EP.
pub(crate) fn gpu_kit(ort: &Path) -> bool {
    let name = format!(
        "{}onnxruntime_providers_cuda{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    );
    ort.parent().is_some_and(|d| d.join(name).is_file())
}

fn have_model(models_dir: &Path, id: &str) -> bool {
    models_dir.join(id).join("model.toml").is_file()
}

/// Установлена ли модель, которую попробует [`load`] (`ml.embed_model` или одна из моделей
/// по умолчанию): дешёвая проверка до загрузки, чтобы запускать Qdrant параллельно с ней.
pub fn model_installed(cfg: &ServerConfig) -> bool {
    let dir = cfg.ml.models_dir(&cfg.data_dir);
    match cfg
        .ml
        .embed_model
        .as_deref()
        .filter(|m| !m.trim().is_empty())
    {
        Some(m) => have_model(&dir, m),
        None => have_model(&dir, MODEL_CPU) || have_model(&dir, MODEL_GPU),
    }
}

/// Загружает модель (блокирующий вызов: sha256 файла модели, сборка сессий).
pub fn load(cfg: &ServerConfig) -> MlLoad {
    let fail = MlLoad::failed;
    let mc = match ml_config(cfg) {
        Ok(c) => c,
        Err(e) => return fail(DegradedReason::NoModel, e.to_string()),
    };
    let ort = ort_path(&mc.ort_dylib);
    if let Err(e) = zam_ml::init_runtime(&ort) {
        return fail(reason_of(&e), e.to_string());
    }
    let explicit = cfg
        .ml
        .embed_model
        .as_deref()
        .filter(|m| !m.trim().is_empty());
    let first = match explicit {
        Some(m) => m.to_string(),
        None if cfg.ml.device != MlDevice::Cpu
            && gpu_kit(&ort)
            && have_model(&mc.models_dir, MODEL_GPU) =>
        {
            MODEL_GPU.to_string()
        }
        None => MODEL_CPU.to_string(),
    };
    let (mut emb, mut report) = match Embedder::load(&mc, &first) {
        Ok(v) => v,
        // fp16 не загрузилась (GPU-комплект без модели fp16 и т. п.) — пробуем int8
        Err(e) if explicit.is_none() && first == MODEL_GPU => {
            tracing::warn!(error = %e, "модель {MODEL_GPU} не загружена, пробую {MODEL_CPU}");
            match Embedder::load(&mc, MODEL_CPU) {
                Ok(v) => v,
                Err(e) => return fail(reason_of(&e), e.to_string()),
            }
        }
        Err(e) => return fail(reason_of(&e), e.to_string()),
    };
    if explicit.is_none()
        && first == MODEL_GPU
        && report.device == Device::Cpu
        && have_model(&mc.models_dir, MODEL_CPU)
    {
        tracing::info!(
            reason = %report.reason,
            "CUDA недоступна: вместо {MODEL_GPU} на CPU загружается {MODEL_CPU}"
        );
        drop(emb);
        match Embedder::load(&mc, MODEL_CPU) {
            Ok(v) => (emb, report) = v,
            Err(e) => return fail(reason_of(&e), e.to_string()),
        }
    }
    // описатель модели старой установки (манифест обновился) — предупреждение с командой
    if let Some((id, _)) = emb.model_id().split_once('@') {
        crate::models::warn_if_stale(&mc.models_dir, id);
    }
    if mc.unload_after.is_some() {
        tracing::warn!("ml.unload_after задан, но выгрузка по простою пока не реализована");
    }
    MlLoad {
        embedder: Some(Arc::new(emb)),
        device: Some(report),
        error: None,
    }
}

fn reason_of(e: &MlError) -> DegradedReason {
    match e {
        MlError::NoRuntime(_) => DegradedReason::NoRuntime,
        _ => DegradedReason::NoModel,
    }
}

/// `zam_ml::Embedder` как модель воркера индексации.
pub struct EmbedderAdapter(pub Arc<Embedder>);

impl PassageEmbedder for EmbedderAdapter {
    fn model_id(&self) -> &str {
        self.0.model_id()
    }

    fn yield_wait(&self) -> std::time::Duration {
        self.0.yield_wait()
    }

    fn dim(&self) -> usize {
        self.0.dim()
    }

    fn embed_passages(&self, texts: &[&str]) -> Result<Vec<zam_store::Embedding>, String> {
        self.0
            .embed_passages(texts)
            .map(|v| {
                v.into_iter()
                    .map(|e| zam_store::Embedding {
                        dense: e.dense,
                        sparse: e.sparse,
                    })
                    .collect()
            })
            .map_err(|e| e.to_string())
    }

    fn count_tokens(&self, text: &str) -> usize {
        self.0.count_tokens(text)
    }
}
