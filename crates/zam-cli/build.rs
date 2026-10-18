//! Git-хэш для `zam --version`: `ZAM_GIT_SHA` из окружения, иначе `git rev-parse --short=12 HEAD`.
use std::path::Path;
use std::process::Command;

fn watch(p: &Path) {
    if p.exists() {
        println!("cargo:rerun-if-changed={}", p.display());
    }
}

fn main() {
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
