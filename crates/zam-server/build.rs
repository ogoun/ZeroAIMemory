//! Сборочные шаги сервера: git-хэш (`ZAM_GIT_SHA` из окружения, иначе `git rev-parse --short HEAD`,
//! иначе "unknown") и подготовка ассетов WebUI из `web/dist` (см. `prepare_web`).
use std::path::{Path, PathBuf};
use std::process::Command;

/// rerun-if-changed только для существующих путей (иначе пересборка при каждом запуске).
fn watch(p: &Path) {
    if p.exists() {
        println!("cargo:rerun-if-changed={}", p.display());
    }
}

const STUB_INDEX: &str = concat!(
    "<!doctype html>
<html lang=\"ru\"><head><meta charset=\"UTF-8\" />",
    "<base href=\"/\" /><title>ZeroAIMemory</title></head>",
    "<body><p>WebUI не собран: выполните npm run build в web/.</p></body></html>
"
);

/// Копирует `dir` в `dst` без корневого `index.html` (его отдаёт `web.rs` с подстановкой base href).
fn copy_assets(dir: &Path, dst: &Path, root: bool) {
    std::fs::create_dir_all(dst).expect("создать каталог ассетов");
    for e in std::fs::read_dir(dir)
        .expect("прочитать web/dist")
        .flatten()
    {
        let (from, to) = (e.path(), dst.join(e.file_name()));
        if from.is_dir() {
            copy_assets(&from, &to, false);
        } else if !(root && e.file_name() == "index.html") {
            std::fs::copy(&from, &to).expect("скопировать ассет");
        }
    }
}

/// Готовит ассеты WebUI в `OUT_DIR`: исходное дерево не трогается. Нет `web/dist` — заглушка.
fn prepare_web() {
    let dist = Path::new("../../web/dist");
    // Нет web/dist: путь в rerun-if-changed заставлял бы перезапускать скрипт на каждой сборке.
    // После первого `npm run build` выполните `cargo build` с ZAM_WEB_REBUILD=1 (или touch build.rs).
    println!("cargo:rerun-if-env-changed=ZAM_WEB_REBUILD");
    if dist.exists() {
        watch(dist);
    } else {
        println!("cargo:rerun-if-changed=build.rs");
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let assets = out.join("web-assets");
    // Каталог ассетов отслеживается memory-serve: пересоздавать его без нужды нельзя (иначе
    // build-скрипт перезапускался бы на каждой сборке).
    let has_dist = dist.join("index.html").is_file();
    let stale = std::fs::read_dir(&assets).is_ok_and(|mut d| d.next().is_some());
    if has_dist || stale {
        let _ = std::fs::remove_dir_all(&assets);
    }
    let index = if has_dist {
        copy_assets(dist, &assets, true);
        std::fs::read_to_string(dist.join("index.html")).expect("прочитать web/dist/index.html")
    } else {
        std::fs::create_dir_all(&assets).expect("создать каталог ассетов");
        STUB_INDEX.to_string()
    };
    let index_path = out.join("web-index.html");
    if std::fs::read_to_string(&index_path).ok().as_deref() != Some(index.as_str()) {
        std::fs::write(&index_path, index).expect("записать index.html");
    }
    println!("cargo:rustc-env=ZAM_WEB_INDEX={}", index_path.display());
    // Без вывода предупреждений memory-serve; ассеты всегда встраиваются в бинарь.
    // SAFETY: build-скрипт однопоточен.
    unsafe { std::env::set_var("MEMORY_SERVE_QUIET", "1") };
    memory_serve::load_directory_with_embed(&assets, true);
}

fn main() {
    prepare_web();
    println!("cargo:rerun-if-env-changed=ZAM_GIT_SHA");
    let git = Path::new("../../.git");
    // В worktree `.git` — файл: отслеживать нечего, остаётся ZAM_GIT_SHA.
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
