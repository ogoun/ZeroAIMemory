//! Выбор устройства и сборка ORT-сессий (исследование 05 §1.3).
//!
//! CUDA: библиотеки NVIDIA предзагружаются из `<каталог ORT>/cuda/` (туда их кладёт
//! `zam-server cuda fetch`), затем CUDA EP регистрируется с `error_on_failure()`: успешная сборка
//! сессии означает, что EP действительно включён. При любой ошибке обе сессии собираются заново
//! на CPU, причина попадает в [`DeviceReport::reason`].

use std::fmt;
use std::path::Path;
use std::str::FromStr;

use ort::environment::Environment;
use ort::ep::{self, ExecutionProvider as _};
use ort::session::Session;
use ort::session::builder::{GraphOptimizationLevel, PrepackedWeights, SessionBuilder};
use serde::Deserialize;

use crate::MlError;

/// Предпочтение устройства (`ml.device`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DevicePref {
    /// CUDA, если доступна, иначе CPU.
    #[default]
    Auto,
    Cpu,
    /// CUDA; если не поднялась — CPU с предупреждением в логе.
    Cuda,
}

impl FromStr for DevicePref {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "auto" => Ok(DevicePref::Auto),
            "cpu" => Ok(DevicePref::Cpu),
            "cuda" => Ok(DevicePref::Cuda),
            _ => Err(format!(
                "неизвестное устройство '{s}', допустимы auto, cpu, cuda"
            )),
        }
    }
}

/// Устройство, на котором фактически работают сессии.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Device {
    Cpu,
    Cuda { id: i32 },
}

impl fmt::Display for Device {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Device::Cpu => f.write_str("cpu"),
            Device::Cuda { id } => write!(f, "cuda:{id}"),
        }
    }
}

/// Итог выбора устройства: для лога и `/index/status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceReport {
    pub device: Device,
    /// Почему не CUDA (пусто, если CUDA включена).
    pub reason: String,
}

impl From<ort::Error<SessionBuilder>> for MlError {
    fn from(e: ort::Error<SessionBuilder>) -> Self {
        MlError::Ort(e.to_string())
    }
}

/// Пара сессий одной модели: индексация и запросы.
pub(crate) struct Sessions {
    pub index: Session,
    pub query: Session,
    pub report: DeviceReport,
}

/// Номер GPU (`ml.device` пока не выбирает карту).
const CUDA_DEVICE_ID: i32 = 0;

fn session(
    env: &Environment,
    model: &Path,
    threads: usize,
    prepacked: &PrepackedWeights,
    cuda: Option<i32>,
) -> Result<Session, MlError> {
    let mut b = Session::builder(env)?
        .with_optimization_level(GraphOptimizationLevel::All)?
        // 0 — решение ORT (число физических ядер)
        .with_intra_threads(threads)?
        // фоновый сервис: без активного ожидания потоков между вызовами
        .with_intra_op_spinning(false)?
        // перепакованные веса MatMul общие для двух сессий
        .with_prepacked_weights(prepacked)?;
    if let Some(id) = cuda {
        b = b.with_execution_providers([ep::CUDA::default()
            .with_device_id(id)
            .build()
            .error_on_failure()])?;
    }
    Ok(b.commit_from_file(model)?)
}

fn pair(
    env: &Environment,
    model: &Path,
    index_threads: usize,
    query_threads: usize,
    cuda: Option<i32>,
) -> Result<(Session, Session), MlError> {
    let pw = PrepackedWeights::new();
    let index = session(env, model, index_threads, &pw, cuda)?;
    let query = session(env, model, query_threads, &pw, cuda)?;
    Ok((index, query))
}

/// Пробует CUDA: предзагрузка библиотек NVIDIA из `<каталог ORT>/cuda/` и сборка сессий
/// (`build(Some(id))`) с CUDA EP.
fn try_cuda<T>(
    ort_dylib: &Path,
    build: &mut impl FnMut(Option<i32>) -> Result<T, MlError>,
) -> Result<T, String> {
    match ep::CUDA::is_available() {
        Ok(true) => {}
        Ok(false) => return Err("ONNX Runtime собран без CUDA EP (CPU-комплект)".into()),
        Err(e) => return Err(format!("список EP недоступен: {e}")),
    }
    let mut notes = Vec::new();
    let cuda_dir = ort_dylib
        .parent()
        .map_or_else(|| Path::new("cuda").to_path_buf(), |d| d.join("cuda"));
    if cuda_dir.is_dir() {
        if let Err(e) = ep::cuda::preload_dylibs(Some(&cuda_dir), Some(&cuda_dir)) {
            notes.push(format!("библиотеки NVIDIA из {}: {e}", cuda_dir.display()));
        }
    } else {
        notes.push(format!(
            "нет каталога {} (zam-server cuda fetch)",
            cuda_dir.display()
        ));
    }
    // даже без своего каталога CUDA может найтись в системе — пробуем
    build(Some(CUDA_DEVICE_ID)).map_err(|e| {
        notes.push(format!("CUDA EP не включился: {e}"));
        notes.join("; ")
    })
}

/// Собирает сессии (`build(cuda)`: `Some(id)` — с CUDA EP, `None` — CPU) на выбранном
/// устройстве с откатом на CPU; причина отката — в [`DeviceReport::reason`].
fn with_device<T>(
    pref: DevicePref,
    ort_dylib: &Path,
    mut build: impl FnMut(Option<i32>) -> Result<T, MlError>,
) -> Result<(T, DeviceReport), MlError> {
    let reason = match pref {
        DevicePref::Cpu => "ml.device = cpu".to_string(),
        DevicePref::Auto | DevicePref::Cuda => match try_cuda(ort_dylib, &mut build) {
            Ok(v) => {
                let device = Device::Cuda { id: CUDA_DEVICE_ID };
                tracing::info!(%device, "сессии ORT на CUDA");
                return Ok((
                    v,
                    DeviceReport {
                        device,
                        reason: String::new(),
                    },
                ));
            }
            Err(why) => {
                if pref == DevicePref::Cuda {
                    tracing::warn!(reason = %why, "ml.device = cuda, но CUDA недоступна: работаем на CPU");
                } else {
                    tracing::info!(reason = %why, "CUDA недоступна, CPU");
                }
                why
            }
        },
    };
    let v = build(None)?;
    Ok((
        v,
        DeviceReport {
            device: Device::Cpu,
            reason,
        },
    ))
}

/// Собирает пару сессий (индексация и запросы) на выбранном устройстве с откатом на CPU.
pub(crate) fn build_sessions(
    env: &Environment,
    pref: DevicePref,
    ort_dylib: &Path,
    model: &Path,
    index_threads: usize,
    query_threads: usize,
) -> Result<Sessions, MlError> {
    let ((index, query), report) = with_device(pref, ort_dylib, |cuda| {
        pair(env, model, index_threads, query_threads, cuda)
    })?;
    Ok(Sessions {
        index,
        query,
        report,
    })
}

/// Одна сессия (реранкер) на выбранном устройстве с откатом на CPU.
pub(crate) fn build_session(
    env: &Environment,
    pref: DevicePref,
    ort_dylib: &Path,
    model: &Path,
    threads: usize,
) -> Result<(Session, DeviceReport), MlError> {
    with_device(pref, ort_dylib, |cuda| {
        session(env, model, threads, &PrepackedWeights::new(), cuda)
    })
}
