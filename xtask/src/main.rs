//! Вспомогательные задачи сборки: `cargo xtask <команда>`.

mod bundles;
mod changelog;
mod fetch;
mod manifest;
mod package;
mod smoke;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use manifest::{
    CUDA_MANIFEST, CudaManifest, MANIFESTS, MODELS_MANIFEST, Manifest, ModelsManifest, PLATFORMS,
};
use package::Flavor;

#[derive(Parser)]
#[command(name = "xtask")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Скачать зависимости поставки (Qdrant, ONNX Runtime CPU и GPU, pdfium) в target/deps/<платформа>/;
    /// библиотеки NVIDIA не скачиваются (их ставит `zam-server cuda fetch`)
    FetchDeps {
        /// windows-x64 | linux-x64
        #[arg(long)]
        platform: Option<String>,
        /// Скачать и напечатать фактические SHA-256 (только при обновлении версий)
        #[arg(long)]
        print_hashes: bool,
        /// Только ONNX Runtime этой сборки: cpu не скачивает ORT GPU, gpu — ORT CPU
        /// (по умолчанию — оба, они нужны тестам zam-ml)
        #[arg(long, value_enum)]
        flavor: Option<Flavor>,
    },
    /// Скачать модели из deploy/models.toml в target/models/<id>/ с проверкой SHA-256
    /// (по умолчанию — модели комплекта zam-models, bundle = true)
    FetchModels {
        /// Только тестовые модели (role = "test"; для CI)
        #[arg(long)]
        test: bool,
        /// Модель по id (можно повторять)
        #[arg(long = "id", value_name = "ID")]
        ids: Vec<String>,
        /// Согласие с некоммерческой лицензией (CC-BY-NC-4.0) для моделей с requires_license_ack
        #[arg(long)]
        accept_nc_license: bool,
    },
    /// Собрать архивы поставки в target/dist/ и SHA256SUMS (сначала: fetch-deps и release-сборка)
    Package {
        /// windows-x64 | linux-x64
        #[arg(long)]
        platform: String,
        /// cpu — ONNX Runtime CPU; gpu — ONNX Runtime с CUDA EP (без библиотек NVIDIA)
        /// и deploy/cuda.toml. Архив клиента zam собирается только с cpu
        #[arg(long, value_enum, default_value = "cpu")]
        flavor: Flavor,
        /// Версия X.Y.Z[-суффикс]
        #[arg(long)]
        version: String,
        /// Путь к zam-server (по умолчанию target/release/zam-server[.exe])
        #[arg(long)]
        server_bin: Option<PathBuf>,
        /// Путь к zam (по умолчанию target/release/zam[.exe])
        #[arg(long)]
        cli_bin: Option<PathBuf>,
        /// Также zam-models-<ver>-<id>.zip — по архиву на модель с bundle = true из
        /// target/models (сначала: cargo xtask fetch-models)
        #[arg(long)]
        with_models: bool,
        /// Также zam-cuda-<ver>-<платформа> — библиотеки NVIDIA по deploy/cuda.toml
        /// (ставятся в target/deps/<платформа>/onnxruntime-gpu/cuda/, как cuda fetch)
        #[arg(long)]
        cuda: bool,
        /// Не собирать комплекты zam-server и zam (только --with-models и --cuda)
        #[arg(long)]
        no_kit: bool,
    },
    /// Напечатать тело раздела версии из CHANGELOG.md (для текста релиза)
    ChangelogSection {
        /// vX.Y.Z[-суффикс] или X.Y.Z[-суффикс]
        version: String,
    },
    /// Smoke-тест комплекта сервера: распаковка, запуск, проверка API, zam, Qdrant и
    /// векторного поиска на тестовой модели (target/models/minilm-l6-int8)
    Smoke {
        /// Архив zam-server-…-(cpu|gpu).(zip|tar.gz)
        #[arg(long)]
        archive: PathBuf,
        /// Архив zam-cuda той же платформы: распаковывается в комплект перед запуском,
        /// `zam-server cuda fetch` должен ничего не скачать (только GPU-комплект)
        #[arg(long)]
        cuda_archive: Option<PathBuf>,
        /// Требуемое устройство модели: cpu | cuda (без флага не проверяется, только
        /// печатается; у CPU-комплекта всегда cpu)
        #[arg(long, value_enum)]
        expect_device: Option<smoke::DeviceKind>,
    },
    /// Проверка архива zam-cuda: штампы совпадают с deploy/cuda.toml, файлы на месте
    /// (cuda fetch после распаковки ничего не скачает), формат библиотек, лицензии NVIDIA
    SmokeCuda {
        /// Архив zam-cuda-<ver>-<платформа>.(zip|tar.gz)
        #[arg(long)]
        archive: PathBuf,
    },
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask лежит в корне репозитория")
        .to_path_buf()
}

fn load_manifests(root: &Path) -> Result<Vec<Manifest>> {
    MANIFESTS
        .iter()
        .map(|n| {
            Ok(Manifest::load(
                &root.join("deploy").join(format!("{n}.toml")),
            )?)
        })
        .collect()
}

fn fetch_deps(platform: Option<String>, print_hashes: bool, flavor: Option<Flavor>) -> Result<()> {
    let root = repo_root();
    let target = root.join("target");
    let manifests = load_manifests(&root)?;
    for m in &manifests {
        m.validate(print_hashes)?;
    }
    // Библиотеки NVIDIA здесь не скачиваются (это `zam-server cuda fetch`), но манифест проверяется.
    let cuda = CudaManifest::load(&root.join("deploy").join(CUDA_MANIFEST))?;
    cuda.validate()?;
    if let Some(p) = platform.as_deref()
        && !PLATFORMS.contains(&p)
    {
        bail!(
            "неизвестная платформа '{p}', допустимы: {}",
            PLATFORMS.join(", ")
        );
    }

    if print_hashes {
        for m in &manifests {
            for (plat, e) in &m.platform {
                if platform.as_deref().is_some_and(|p| p != plat) {
                    continue;
                }
                println!("# {} {} [{plat}] {}", m.name, m.version, e.url);
                let h = fetch::print_hash(&e.url, &target)?;
                println!("{}.toml [platform.{plat}] sha256 = \"{h}\"", m.name);
            }
        }
        return Ok(());
    }

    let platform =
        platform.context("укажите --platform windows-x64|linux-x64 (или --print-hashes)")?;
    if !PLATFORMS.contains(&platform.as_str()) {
        bail!(
            "неизвестная платформа '{platform}', допустимы: {}",
            PLATFORMS.join(", ")
        );
    }
    let cuda_mb: u64 = cuda
        .lib
        .iter()
        .filter(|l| l.platform == platform)
        .map(|l| l.size)
        .sum::<u64>()
        / 1_000_000;
    println!(
        "cuda.toml: CUDA {}, cuDNN {} [{platform}] — {cuda_mb} МБ архивов NVIDIA \
         не скачиваются (их ставит `zam-server cuda fetch`)",
        cuda.cuda, cuda.cudnn
    );
    for m in &manifests {
        if !needed_for(&m.name, flavor) {
            println!("{} [{platform}]: не нужен этой сборке, пропуск", m.name);
            continue;
        }
        let e = m
            .platform
            .get(&platform)
            .with_context(|| format!("{}: нет платформы {platform} в манифесте", m.name))?;
        let out = target.join("deps").join(&platform).join(&m.name);
        let cached = fetch::fetch_one(&m.name, e, &target, &out)?;
        println!(
            "{} {} [{platform}]: {} -> {}",
            m.name,
            m.version,
            if cached {
                "из кэша"
            } else {
                "загружено"
            },
            out.display()
        );
    }
    Ok(())
}

/// Нужен ли манифест зависимостей сборке: ORT CPU — только cpu, ORT GPU — только gpu; без
/// `--flavor` нужны оба (тестам zam-ml).
fn needed_for(name: &str, flavor: Option<Flavor>) -> bool {
    !matches!(
        (name, flavor),
        ("onnxruntime-gpu", Some(Flavor::Cpu)) | ("onnxruntime", Some(Flavor::Gpu))
    )
}

fn fetch_models(ids: Vec<String>, test: bool, accept_nc: bool) -> Result<()> {
    let root = repo_root();
    let manifest = ModelsManifest::load(&root.join("deploy").join(MODELS_MANIFEST))?;
    manifest.validate()?;
    let models = root.join("target").join("models");
    // согласие с лицензией проверяется для всего набора до первой загрузки
    for m in manifest.select(&ids, test, accept_nc)? {
        println!(
            "{} ({:?}/{:?}, {}, {:.1} МБ): {}",
            m.id,
            m.role,
            m.device,
            m.license,
            m.total_size() as f64 / 1e6,
            models.join(&m.id).display()
        );
        let got = zam_fetch::fetch_model(&manifest, &m.id, &models, accept_nc)
            .with_context(|| m.id.clone())?;
        for (path, cached) in &got.files {
            println!(
                "  {path}: {}",
                if *cached {
                    "на месте"
                } else {
                    "загружено"
                }
            );
        }
        println!(
            "  {}, {}: записаны",
            zam_fetch::MODEL_LICENSE_FILE,
            zam_fetch::MODEL_SPEC_FILE
        );
    }
    Ok(())
}

/// Аргументы `cargo xtask package`.
struct PackageArgs {
    platform: String,
    flavor: Flavor,
    version: String,
    server_bin: Option<PathBuf>,
    cli_bin: Option<PathBuf>,
    with_models: bool,
    cuda: bool,
    no_kit: bool,
}

fn package_cmd(a: PackageArgs) -> Result<()> {
    let platform = a.platform;
    if !PLATFORMS.contains(&platform.as_str()) {
        bail!(
            "неизвестная платформа '{platform}', допустимы: {}",
            PLATFORMS.join(", ")
        );
    }
    package::validate_version(&a.version)?;
    if a.no_kit && !a.with_models && !a.cuda {
        bail!("--no-kit без --with-models и --cuda: собирать нечего");
    }
    let root = repo_root();
    let target = root.join("target");
    let deploy = root.join("deploy");
    let dist = target.join("dist");
    let deps = target.join("deps").join(&platform);
    let mut archives = Vec::new();
    if !a.no_kit {
        let release = target.join("release");
        let server_bin = a
            .server_bin
            .unwrap_or_else(|| release.join(package::exe(&platform, "zam-server")));
        let cli_bin = a
            .cli_bin
            .unwrap_or_else(|| release.join(package::exe(&platform, "zam")));
        let licenses = target.join("licenses");
        package::generate_licenses(&root, &licenses)?;
        let crt = if platform.starts_with("windows") {
            Some(package::find_crt_dir(a.flavor)?)
        } else {
            None
        };
        let qv = package::qdrant_version(&deploy)?;
        let license = root.join("LICENSE");
        archives.extend(package::package(&package::Inputs {
            platform: &platform,
            version: &a.version,
            flavor: a.flavor,
            server_bin: &server_bin,
            cli_bin: &cli_bin,
            deps_dir: &deps,
            deploy_dir: &deploy,
            generated_licenses_dir: &licenses,
            project_license: &license,
            crt_dir: crt.as_deref(),
            qdrant_version: &qv,
            out_dir: &dist,
        })?);
    }
    if a.with_models {
        archives.extend(bundles::package_models(
            &deploy.join(MODELS_MANIFEST),
            &target.join("models"),
            &a.version,
            &dist,
        )?);
    }
    if a.cuda {
        archives.push(bundles::package_cuda(
            &deploy.join(CUDA_MANIFEST),
            &deploy.join("licenses").join("nvidia"),
            &platform,
            &deps.join("onnxruntime-gpu").join(bundles::CUDA_DIR),
            &a.version,
            &dist,
        )?);
    }
    let sums = package::write_sha256sums(&dist, &archives)?;
    for p in archives.iter().chain([&sums]) {
        let len = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        println!("{} ({:.1} МБ)", p.display(), len as f64 / 1e6);
    }
    Ok(())
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::FetchDeps {
            platform,
            print_hashes,
            flavor,
        } => fetch_deps(platform, print_hashes, flavor),
        Cmd::FetchModels {
            test,
            ids,
            accept_nc_license,
        } => fetch_models(ids, test, accept_nc_license),
        Cmd::Package {
            platform,
            flavor,
            version,
            server_bin,
            cli_bin,
            with_models,
            cuda,
            no_kit,
        } => package_cmd(PackageArgs {
            platform,
            flavor,
            version,
            server_bin,
            cli_bin,
            with_models,
            cuda,
            no_kit,
        }),
        Cmd::ChangelogSection { version } => {
            changelog::run(&repo_root().join("CHANGELOG.md"), &version)
        }
        Cmd::Smoke {
            archive,
            cuda_archive,
            expect_device,
        } => smoke::run(
            &archive,
            cuda_archive.as_deref(),
            expect_device,
            &repo_root(),
        ),
        Cmd::SmokeCuda { archive } => smoke::run_cuda(&archive, &repo_root()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flavor_fetches_only_its_runtime() {
        assert!(!needed_for("onnxruntime-gpu", Some(Flavor::Cpu)));
        assert!(needed_for("onnxruntime-gpu", Some(Flavor::Gpu)));
        assert!(needed_for("onnxruntime-gpu", None));
        assert!(needed_for("onnxruntime", Some(Flavor::Cpu)));
        assert!(!needed_for("onnxruntime", Some(Flavor::Gpu)));
        assert!(needed_for("onnxruntime", None));
        assert!(needed_for("qdrant", Some(Flavor::Gpu)));
    }
}
