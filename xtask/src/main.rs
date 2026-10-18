//! Вспомогательные задачи сборки: `cargo xtask <команда>`.

mod changelog;
mod fetch;
mod manifest;
mod package;
mod smoke;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use manifest::{MANIFESTS, Manifest, PLATFORMS};

#[derive(Parser)]
#[command(name = "xtask")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Скачать зависимости поставки (Qdrant, ONNX Runtime, pdfium) в target/deps/<платформа>/
    FetchDeps {
        /// windows-x64 | linux-x64
        #[arg(long)]
        platform: Option<String>,
        /// Скачать и напечатать фактические SHA-256 (только при обновлении версий)
        #[arg(long)]
        print_hashes: bool,
    },
    /// Собрать архивы поставки в target/dist/ и SHA256SUMS (сначала: fetch-deps и release-сборка)
    Package {
        /// windows-x64 | linux-x64
        #[arg(long)]
        platform: String,
        /// cpu (gpu появится на этапе M2)
        #[arg(long, default_value = "cpu")]
        flavor: String,
        /// Версия X.Y.Z[-суффикс]
        #[arg(long)]
        version: String,
        /// Путь к zam-server (по умолчанию target/release/zam-server[.exe])
        #[arg(long)]
        server_bin: Option<PathBuf>,
        /// Путь к zam (по умолчанию target/release/zam[.exe])
        #[arg(long)]
        cli_bin: Option<PathBuf>,
    },
    /// Напечатать тело раздела версии из CHANGELOG.md (для текста релиза)
    ChangelogSection {
        /// vX.Y.Z[-суффикс] или X.Y.Z[-суффикс]
        version: String,
    },
    /// Smoke-тест комплекта сервера: распаковка, запуск, проверка API, zam и Qdrant
    Smoke {
        /// Архив zam-server-…-cpu.(zip|tar.gz)
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
        .map(|n| Manifest::load(&root.join("deploy").join(format!("{n}.toml"))))
        .collect()
}

fn fetch_deps(platform: Option<String>, print_hashes: bool) -> Result<()> {
    let root = repo_root();
    let target = root.join("target");
    let manifests = load_manifests(&root)?;
    for m in &manifests {
        m.validate(print_hashes)?;
    }
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
    for m in &manifests {
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

fn package_cmd(
    platform: String,
    flavor: String,
    version: String,
    server_bin: Option<PathBuf>,
    cli_bin: Option<PathBuf>,
) -> Result<()> {
    if flavor == "gpu" {
        bail!("GPU-комплект появится на этапе M2");
    }
    if flavor != "cpu" {
        bail!("неизвестный flavor '{flavor}', допустим cpu");
    }
    if !PLATFORMS.contains(&platform.as_str()) {
        bail!(
            "неизвестная платформа '{platform}', допустимы: {}",
            PLATFORMS.join(", ")
        );
    }
    let root = repo_root();
    let target = root.join("target");
    let release = target.join("release");
    let server_bin =
        server_bin.unwrap_or_else(|| release.join(package::exe(&platform, "zam-server")));
    let cli_bin = cli_bin.unwrap_or_else(|| release.join(package::exe(&platform, "zam")));
    let deploy = root.join("deploy");
    let licenses = target.join("licenses");
    package::generate_licenses(&root, &licenses)?;
    let vc = if platform.starts_with("windows") {
        Some(package::find_vcruntime()?)
    } else {
        None
    };
    let qv = package::qdrant_version(&deploy)?;
    let dist = target.join("dist");
    let license = root.join("LICENSE");
    let deps = target.join("deps").join(&platform);
    let (server, cli) = package::package(&package::Inputs {
        platform: &platform,
        version: &version,
        server_bin: &server_bin,
        cli_bin: &cli_bin,
        deps_dir: &deps,
        deploy_dir: &deploy,
        generated_licenses_dir: &licenses,
        project_license: &license,
        vcruntime: vc.as_deref(),
        qdrant_version: &qv,
        out_dir: &dist,
    })?;
    let sums = package::write_sha256sums(&dist, &[server.clone(), cli.clone()])?;
    for p in [server, cli, sums] {
        println!("{}", p.display());
    }
    Ok(())
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::FetchDeps {
            platform,
            print_hashes,
        } => fetch_deps(platform, print_hashes),
        Cmd::Package {
            platform,
            flavor,
            version,
            server_bin,
            cli_bin,
        } => package_cmd(platform, flavor, version, server_bin, cli_bin),
        Cmd::ChangelogSection { version } => {
            changelog::run(&repo_root().join("CHANGELOG.md"), &version)
        }
        Cmd::Smoke { archive } => smoke::run(&archive, &repo_root().join("target")),
    }
}
