//! Git-хэш для `zam --version`: `ZAM_GIT_SHA` из окружения, иначе `git rev-parse --short=12 HEAD`.
//!
//! Стек главного потока `zam` на Windows (MSVC) — 8 МБ, как у главного потока Linux (M6.5,
//! задача 6): clap derive строит дерево команд целиком при любом вызове (`zam --version` тоже),
//! а в отладочной сборке кадры `augment_subcommands` не переиспользуют слоты временных `Arg`.
//! С `zam idea review` разбор аргументов потребовал ≈ 1,08 МБ при 1 МБ по умолчанию у
//! компоновщика MSVC — `STATUS_STACK_OVERFLOW` до первой строки `main`. Размер — резерв
//! адресного пространства, память выделяется по мере роста стека: холодный старт не меняется.
use std::path::Path;
use std::process::Command;

/// Резерв стека главного потока `zam` на Windows, байт.
const WINDOWS_MAIN_STACK: u32 = 8 * 1024 * 1024;

fn watch(p: &Path) {
    if p.exists() {
        println!("cargo:rerun-if-changed={}", p.display());
    }
}

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        println!("cargo:rustc-link-arg-bin=zam=/STACK:{WINDOWS_MAIN_STACK}");
    }
    println!("cargo:rerun-if-env-changed=ZAM_GIT_SHA");
    let git = Path::new("../../.git");
    if git.is_dir() {
        let head = git.join("HEAD");
        watch(&head);
        if let Ok(text) = std::fs::read_to_string(&head)
            && let Some(r) = text.trim().strip_prefix("ref: ")
        {
            watch(&git.join(r));
        }
        watch(&git.join("packed-refs"));
    }
    let sha = std::env::var("ZAM_GIT_SHA")
        .ok()
        .map(|s| s.trim().chars().take(12).collect::<String>())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            Command::new("git")
                .args(["rev-parse", "--short=12", "HEAD"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=ZAM_GIT_SHA={sha}");
}
