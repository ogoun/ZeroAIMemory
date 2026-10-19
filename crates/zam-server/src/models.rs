//! Команды `zam-server models fetch|status` (§8.4): модели из `deploy/models.toml`
//! (встроен при сборке, `--manifest` — свой) в `ml.models_dir` (`<data_dir>/models`).
//!
//! Разбор манифеста, проверка и загрузка — общие с `cargo xtask fetch-models` (zam-fetch).
//! Набор по умолчанию — модель эмбеддингов устройства (`--device`, иначе по `ml.device` и
//! комплекту: рядом с ORT есть CUDA EP — gpu), с `--reranker` — ещё реранкер устройства
//! (`ml.rerank_model_cpu|gpu`). Реранкер jina (CC-BY-NC-4.0) без `--accept-nc-license` не
//! скачивается: команда завершается с текстом о лицензии до первой загрузки.

use std::path::Path;

use anyhow::{Context, bail};
use zam_fetch::{
    ModelEntry, ModelRole, ModelState, ModelsManifest, check_license_ack, fetch_model_with,
    model_state,
};

use crate::config::{MlDevice, ServerConfig};
use crate::ml::{MODEL_CPU, MODEL_GPU, gpu_kit, ml_config, ort_path};

/// Манифест моделей, с которым собран сервер.
const BUILT_IN_MANIFEST: &str = include_str!("../../../deploy/models.toml");

/// Устройство набора моделей.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum FetchDevice {
    Cpu,
    Gpu,
}

/// Манифест: явный путь, иначе встроенный; всегда проверяется (https, ревизии, хэши).
pub fn load_manifest(path: Option<&Path>) -> anyhow::Result<ModelsManifest> {
    let m = match path {
        Some(p) => ModelsManifest::load(p)?,
        None => ModelsManifest::parse(BUILT_IN_MANIFEST)?,
    };
    m.validate()?;
    Ok(m)
}

/// Устройство по умолчанию: `ml.device = cpu` → cpu; иначе gpu, если рядом с ORT лежит
/// CUDA EP (GPU-комплект), иначе cpu.
pub fn default_device(cfg: &ServerConfig) -> FetchDevice {
    if cfg.ml.device == MlDevice::Cpu {
        return FetchDevice::Cpu;
    }
    match ml_config(cfg) {
        Ok(mc) if gpu_kit(&ort_path(&mc.ort_dylib)) => FetchDevice::Gpu,
        _ => FetchDevice::Cpu,
    }
}

/// Что качать: явные `ids`, иначе эмбеддер устройства (или `ml.embed_model`) и с
/// `reranker` — реранкер устройства.
pub fn plan<'a>(
    m: &'a ModelsManifest,
    cfg: &ServerConfig,
    device: FetchDevice,
    reranker: bool,
    ids: &[String],
) -> anyhow::Result<Vec<&'a ModelEntry>> {
    let mut want: Vec<String> = ids.to_vec();
    if want.is_empty() {
        let embed = cfg
            .ml
            .embed_model
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                match device {
                    FetchDevice::Cpu => MODEL_CPU,
                    FetchDevice::Gpu => MODEL_GPU,
                }
                .to_string()
            });
        want.push(embed);
    }
    if reranker {
        want.push(match device {
            FetchDevice::Cpu => cfg.ml.rerank_model_cpu.clone(),
            FetchDevice::Gpu => cfg.ml.rerank_model_gpu.clone(),
        });
    }
    let mut out: Vec<&ModelEntry> = Vec::new();
    for id in &want {
        let e = m.get(id)?;
        // fp32-эталон — инструмент разработки (`cargo xtask eval --quality-gate`), не модель
        // для сервера: 2,3 ГБ без выигрыша против int8/fp16
        if e.role == ModelRole::Reference {
            bail!(
                "{}: эталон качества для eval (cargo xtask eval --quality-gate), а не модель \n                 сервера",
                e.id
            );
        }
        if !out.iter().any(|x| x.id == e.id) {
            out.push(e);
        }
    }
    if reranker
        && let Some(e) = out.last()
        && e.role != ModelRole::Rerank
    {
        bail!("ml.rerank_model_*: '{}' не реранкер", e.id);
    }
    Ok(out)
}

fn mb(b: u64) -> String {
    format!("{:.1} МБ", b as f64 / 1e6)
}

/// `models fetch`: согласие с лицензией — для всего набора до первой загрузки; затем
/// модель за моделью (файлы на месте с верным хэшем не перекачиваются). `log` — строки
/// прогресса.
pub fn fetch(
    m: &ModelsManifest,
    list: &[&ModelEntry],
    models_dir: &Path,
    accept_nc: bool,
    log: &mut dyn FnMut(&str),
) -> anyhow::Result<()> {
    check_license_ack(list, accept_nc)?;
    for e in list {
        log(&format!(
            "{} ({}, {}, {}): {}",
            e.id,
            format!("{:?}/{:?}", e.role, e.device).to_lowercase(),
            e.license,
            mb(e.total_size()),
            models_dir.join(&e.id).display()
        ));
        let got = fetch_model_with(m, &e.id, models_dir, accept_nc, &mut |f| {
            log(&format!("  {}: {} …", f.path, mb(f.size)));
        })
        .with_context(|| format!("модель {}", e.id))?;
        let fresh = got.files.iter().filter(|(_, cached)| !*cached).count();
        log(&format!(
            "  готово: загружено файлов {fresh}, уже на месте {}; SHA-256 сверены",
            got.files.len() - fresh
        ));
        if e.requires_license_ack {
            log(&format!(
                "  лицензия {}: только некоммерческое использование (см. {}/LICENSE)",
                e.license,
                got.dir.display()
            ));
        }
    }
    Ok(())
}

/// `models status`: модели манифеста и их состояние в `models_dir`; какие модели возьмёт
/// сервер. С `verify` сверяются SHA-256 (долго для гигабайтных весов).
pub fn status(m: &ModelsManifest, cfg: &ServerConfig, verify: bool) -> String {
    let dir = cfg.ml.models_dir(&cfg.data_dir);
    let mut out = vec![format!("каталог моделей: {}", dir.display())];
    // эталон для eval пользователю не предлагается
    for e in m.model.iter().filter(|e| e.role != ModelRole::Reference) {
        let state = match model_state(e, &dir, verify) {
            ModelState::Installed if verify => "установлена (SHA-256 сверены)".to_string(),
            ModelState::Installed => "установлена".to_string(),
            ModelState::Missing => "нет".to_string(),
            ModelState::Broken(why) => format!("неполная: {why}"),
        };
        let pack = if e.requires_license_ack {
            ", по согласию (--accept-nc-license)"
        } else if e.bundle {
            ", zam-models"
        } else {
            ""
        };
        out.push(format!(
            "{:<20} {:<12} {:<13} {:>10}{pack}: {state}",
            e.id,
            format!("{:?}/{:?}", e.role, e.device).to_lowercase(),
            e.license,
            mb(e.total_size()),
        ));
    }
    let device = default_device(cfg);
    out.push(format!(
        "устройство набора по умолчанию: {}; реранкер: CPU — {}, GPU — {}",
        match device {
            FetchDevice::Cpu => "cpu",
            FetchDevice::Gpu => "gpu",
        },
        cfg.ml.rerank_model_cpu,
        cfg.ml.rerank_model_gpu
    ));
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_manifest_and_plans() {
        let m = load_manifest(None).unwrap();
        let mut cfg = ServerConfig::default();
        let ids = |v: Vec<&ModelEntry>| v.iter().map(|e| e.id.clone()).collect::<Vec<_>>();
        assert_eq!(
            ids(plan(&m, &cfg, FetchDevice::Cpu, false, &[]).unwrap()),
            ["bge-m3-int8"]
        );
        assert_eq!(
            ids(plan(&m, &cfg, FetchDevice::Cpu, true, &[]).unwrap()),
            ["bge-m3-int8", "jina-reranker-v2"]
        );
        assert_eq!(
            ids(plan(&m, &cfg, FetchDevice::Gpu, true, &[]).unwrap()),
            ["bge-m3-fp16", "bge-reranker-v2-m3"]
        );
        assert_eq!(
            ids(plan(
                &m,
                &cfg,
                FetchDevice::Gpu,
                false,
                &["minilm-l6-int8".into()]
            )
            .unwrap()),
            ["minilm-l6-int8"]
        );
        cfg.ml.embed_model = Some("minilm-l6-int8".into());
        assert_eq!(
            ids(plan(&m, &cfg, FetchDevice::Cpu, false, &[]).unwrap()),
            ["minilm-l6-int8"]
        );
        // реранкер в настройке — не реранкер
        cfg.ml.rerank_model_cpu = "bge-m3-int8".into();
        assert!(plan(&m, &cfg, FetchDevice::Cpu, true, &[]).is_err());
        assert!(plan(&m, &cfg, FetchDevice::Cpu, false, &["nope".into()]).is_err());
        // fp32-эталон eval сервер не ставит и в status не показывает
        let err = plan(&m, &cfg, FetchDevice::Cpu, false, &["bge-m3-fp32".into()])
            .unwrap_err()
            .to_string();
        assert!(err.contains("eval"), "{err}");
        assert!(!status(&m, &cfg, false).contains("bge-m3-fp32"));
        cfg.ml.device = MlDevice::Cpu;
        assert_eq!(default_device(&cfg), FetchDevice::Cpu);
    }

    #[test]
    fn jina_without_ack_fails_before_download() {
        let m = load_manifest(None).unwrap();
        let cfg = ServerConfig::default();
        let list = plan(&m, &cfg, FetchDevice::Cpu, true, &[]).unwrap();
        let dir = crate::test_util::dir_in_target("models-nc");
        let _ = std::fs::remove_dir_all(&dir);
        let mut lines = Vec::new();
        let err = fetch(&m, &list, &dir, false, &mut |l| lines.push(l.to_string()))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("CC-BY-NC-4.0") && err.contains("--accept-nc-license"),
            "{err}"
        );
        // ничего не скачано и не создано
        assert!(lines.is_empty(), "{lines:?}");
        assert!(!dir.exists());
    }
}
