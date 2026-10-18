//! Git-хэш сборки: `ZAM_GIT_SHA` из окружения, иначе `git rev-parse --short HEAD`, иначе "unknown".
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=ZAM_GIT_SHA");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    let sha = std::env::var("ZAM_GIT_SHA")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            Command::new("git")
                .args(["rev-parse", "--short", "HEAD"])
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
