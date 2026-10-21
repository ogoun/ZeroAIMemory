//! Команда `zam-server cuda fetch` (§8.4, §16.3): библиотеки NVIDIA (CUDA 13, cuDNN 9) для
//! CUDA EP ONNX Runtime по `deploy/cuda.toml` (встроен при сборке, `--manifest` — свой).
//!
//! В GPU-комплект они не входят (решение владельца). Файлы кладутся плоско в
//! `<каталог ORT>/cuda/` — оттуда их предзагружает zam-ml перед включением CUDA EP. Каждый
//! архив сверяется по размеру и SHA-256 манифеста, рядом пишется штамп `.<компонент>.sha256`.
//! Архив пропускается, только если штамп совпал с манифестом и файлы на месте; иначе (новый
//! выпуск в манифесте, файлы без штампа) — перекачка и замена.
//! Linux: tar.xz, имена SONAME — файлы, на которые указывают ссылки архива.

use std::path::{Path, PathBuf};

use anyhow::bail;
use zam_fetch::{CudaManifest, host_platform, install_cuda};

use crate::config::ServerConfig;
use crate::ml::{gpu_kit, ml_config, ort_path};

/// Манифест библиотек NVIDIA, с которым собран сервер.
const BUILT_IN_MANIFEST: &str = include_str!("../../../deploy/cuda.toml");

/// Лицензия библиотек NVIDIA (CUDA Toolkit EULA, cuDNN SLA).
const NVIDIA_EULA: &str = "https://docs.nvidia.com/cuda/eula/index.html";

/// Манифест: явный путь, иначе встроенный; всегда проверяется (https на
/// developer.download.nvidia.com, sha256, размеры, имена).
pub fn load_manifest(path: Option<&Path>) -> anyhow::Result<CudaManifest> {
    let m = match path {
        Some(p) => CudaManifest::load(p)?,
        None => CudaManifest::parse(BUILT_IN_MANIFEST)?,
    };
    m.validate()?;
    Ok(m)
}

/// Библиотека ORT сервера и каталог `<каталог ORT>/cuda/`.
pub fn cuda_dir(cfg: &ServerConfig) -> anyhow::Result<(PathBuf, PathBuf)> {
    let ort = ort_path(&ml_config(cfg)?.ort_dylib);
    let dir = ort
        .parent()
        .map(|d| d.join("cuda"))
        .ok_or_else(|| anyhow::anyhow!("у пути {} нет каталога", ort.display()))?;
    Ok((ort, dir))
}

/// Итог `cuda fetch` (`--json` печатает его как есть).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FetchReport {
    /// Каталог библиотек (`<каталог ORT>/cuda/`).
    pub dir: PathBuf,
    /// Файлов библиотек платформы по манифесту.
    pub files: usize,
    /// Архивов NVIDIA скачано (размер и SHA-256 сверены).
    pub downloaded: usize,
    /// Архивов уже установлено (штамп совпал с манифестом) — без загрузки.
    pub installed: usize,
}

impl FetchReport {
    /// Текст для человека.
    pub fn message(&self) -> String {
        format!(
            "библиотеки NVIDIA: {} файлов в {}; загружено архивов {} (размер и SHA-256 \
             сверены), уже установлено {} (штамп совпал).\nНужен драйвер NVIDIA с поддержкой \
             CUDA 13 (ветка R580 и новее). Лицензия библиотек NVIDIA: {NVIDIA_EULA}\nПерезапустите \
             zam-server: устройство и причина выбора — в журнале и /index/status.",
            self.files,
            self.dir.display(),
            self.downloaded,
            self.installed
        )
    }
}

/// `cuda fetch`: ставит библиотеки платформы в `<каталог ORT>/cuda/`. Без CUDA EP рядом с
/// ORT (CPU-комплект) — ошибка: CUDA ему не нужна. `log` — строки прогресса.
pub fn fetch(
    cfg: &ServerConfig,
    m: &CudaManifest,
    log: &mut dyn FnMut(&str),
) -> anyhow::Result<FetchReport> {
    let (ort, dir) = cuda_dir(cfg)?;
    if !gpu_kit(&ort) {
        bail!(
            "рядом с {} нет onnxruntime_providers_cuda: это CPU-комплект, ему библиотеки CUDA \
             не нужны. Поставьте GPU-комплект zam-server или задайте ml.ort_dylib на ONNX \
             Runtime с CUDA EP",
            ort.display()
        );
    }
    let platform = host_platform();
    log(&format!(
        "CUDA {}, cuDNN {} [{platform}]: {} архивов, {:.0} МБ → {}",
        m.cuda,
        m.cudnn,
        m.libs(platform).count(),
        m.total_size(platform) as f64 / 1e6,
        dir.display()
    ));
    let done = install_cuda(m, platform, &dir, &mut |l, installed| {
        if installed {
            log(&format!(
                "  {} {}: уже установлено (штамп совпал)",
                l.component, l.version
            ));
        } else {
            log(&format!(
                "  {} {} ({:.0} МБ) …",
                l.component,
                l.version,
                l.size as f64 / 1e6
            ));
        }
    })?;
    let fresh = done.iter().filter(|(_, cached)| !*cached).count();
    Ok(FetchReport {
        files: m.dest_names(platform)?.len(),
        dir,
        downloaded: fresh,
        installed: done.len() - fresh,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_manifest_is_valid_and_cpu_kit_refuses() {
        let m = load_manifest(None).unwrap();
        assert!(m.libs(host_platform()).count() > 0);
        let mut cfg = ServerConfig::default();
        // CPU-сборка ORT из target/deps: рядом нет CUDA EP
        let dir = crate::test_util::dir_in_target("cuda-cpu-kit");
        cfg.ml.ort_dylib = Some(dir.join("onnxruntime.dll"));
        let (_, cuda) = cuda_dir(&cfg).unwrap();
        assert_eq!(cuda, dir.join("cuda"));
        let mut lines = Vec::new();
        let err = fetch(&cfg, &m, &mut |l| lines.push(l.to_string()))
            .unwrap_err()
            .to_string();
        assert!(err.contains("CPU-комплект"), "{err}");
        assert!(lines.is_empty() && !cuda.exists());
    }
}
