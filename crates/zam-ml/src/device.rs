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
        // 0 — решение ORT (число физических ядер); маска CPU не {0..n-1} (LXC, cpuset) — явно
        .with_intra_threads(intra_threads(threads))?
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

/// Число потоков intra-op для ORT. `0` — решение ORT: по потоку на физическое ядро **с
/// привязкой** к CPU с номерами `0, 1, …`. Если маска процесса — не `{0, 1, …, n−1}`
/// (cpuset с частью ядер, LXC с «чужими» номерами CPU: стенд Debian 13 — `39,55`), ORT
/// привязывает потоки к CPU вне маски и пишет ERROR `pthread_setaffinity_np failed … error
/// code: 22`. При явном числе потоков ORT привязку не ставит (её задаёт только
/// `session.intra_op_thread_affinities`) — поэтому для такой маски число задаётся явно: число
/// CPU в маске **[решение контроллера, fix round 1 задачи 14 M4, M-8]**. На обычной машине маска
/// `{0..n−1}`, на Windows маска не читается — поведение ORT прежнее.
fn intra_threads(requested: usize) -> usize {
    if requested != 0 {
        return requested;
    }
    own_cpus().map_or(0, |cpus| explicit_threads_for(&cpus))
}

/// `0` (решает ORT), если CPU маски — ровно `0..n`; иначе число CPU маски.
fn explicit_threads_for(cpus: &[usize]) -> usize {
    let mut v = cpus.to_vec();
    v.sort_unstable();
    v.dedup();
    if v.iter().enumerate().all(|(i, &c)| i == c) {
        0
    } else {
        v.len()
    }
}

/// Маска CPU процесса (`Cpus_allowed_list` из `/proc/self/status`, как `sched_getaffinity`).
#[cfg(target_os = "linux")]
fn own_cpus() -> Option<Vec<usize>> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = s.lines().find(|l| l.starts_with("Cpus_allowed_list:"))?;
    parse_cpu_list(line.split_once(':')?.1.trim())
}

#[cfg(not(target_os = "linux"))]
fn own_cpus() -> Option<Vec<usize>> {
    None
}

/// Список CPU ядра Linux (`0-3,8-11,15`) → номера CPU.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_cpu_list(list: &str) -> Option<Vec<usize>> {
    let mut v = Vec::new();
    for part in list.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        match part.split_once('-') {
            Some((a, b)) => {
                let (a, b): (usize, usize) = (a.parse().ok()?, b.parse().ok()?);
                if b < a || b - a > 65_536 {
                    return None;
                }
                v.extend(a..=b);
            }
            None => v.push(part.parse().ok()?),
        }
    }
    (!v.is_empty()).then_some(v)
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

/// `count` сессий одной модели (реранкер) на выбранном устройстве с откатом на CPU; по
/// `threads` потоков у каждой, перепакованные веса MatMul общие (`PrepackedWeights`).
pub(crate) fn build_session_pool(
    env: &Environment,
    pref: DevicePref,
    ort_dylib: &Path,
    model: &Path,
    threads: usize,
    count: usize,
) -> Result<(Vec<Session>, DeviceReport), MlError> {
    with_device(pref, ort_dylib, |cuda| {
        let pw = PrepackedWeights::new();
        (0..count.max(1))
            .map(|_| session(env, model, threads, &pw, cuda))
            .collect()
    })
}

#[cfg(test)]
mod affinity_tests {
    use super::*;

    /// Список CPU ядра Linux (`Cpus_allowed_list`).
    #[test]
    fn cpu_list_parsed() {
        assert_eq!(parse_cpu_list("0-3"), Some(vec![0, 1, 2, 3]));
        assert_eq!(parse_cpu_list("39,55"), Some(vec![39, 55]));
        assert_eq!(parse_cpu_list("0-1,8,10-11"), Some(vec![0, 1, 8, 10, 11]));
        assert_eq!(parse_cpu_list(""), None);
        assert_eq!(parse_cpu_list("3-1"), None);
        assert_eq!(parse_cpu_list("x"), None);
    }

    /// M-8: маска `{0..n−1}` — решает ORT (`0`), иначе явно число CPU маски.
    #[test]
    fn explicit_threads_when_mask_not_prefix() {
        assert_eq!(explicit_threads_for(&[39, 55]), 2);
        assert_eq!(explicit_threads_for(&[0, 1]), 0);
        assert_eq!(explicit_threads_for(&[0, 2]), 2);
        assert_eq!(explicit_threads_for(&[1, 0, 2, 3]), 0);
        assert_eq!(explicit_threads_for(&[2, 3]), 2);
    }

    /// Явное число потоков не меняется.
    #[test]
    fn explicit_threads_kept() {
        assert_eq!(intra_threads(4), 4);
        let auto = intra_threads(0);
        assert_eq!(auto, own_cpus().map_or(0, |c| explicit_threads_for(&c)));
    }
}
