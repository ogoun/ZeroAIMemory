//! Вспомогательные задачи сборки: `cargo xtask <команда>`.

mod fetch;
mod manifest;

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

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::FetchDeps {
            platform,
            print_hashes,
        } => fetch_deps(platform, print_hashes),
    }
}
