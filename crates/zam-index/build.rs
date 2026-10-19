//! Закреплённая версия Qdrant из манифеста поставки `deploy/qdrant.toml`.
//!
//! Версия нужна супервизору на этапе компиляции: путь бинарника по умолчанию
//! `<exe_dir>/qdrant/<ver>/`, сверка `GET /` и метка хранилища. Источник один —
//! манифест; здесь разбирается только верхнеуровневый ключ `version`, чтобы не
//! тянуть парсер TOML в build-зависимости.

use std::path::Path;

fn main() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qdrant.toml");
    println!("cargo:rerun-if-changed={}", manifest.display());
    let text = std::fs::read_to_string(&manifest)
        .unwrap_or_else(|e| panic!("не удалось прочитать {}: {e}", manifest.display()));
    let version = top_level_version(&text)
        .unwrap_or_else(|| panic!("в {} нет ключа version", manifest.display()));
    assert!(
        version.split('.').count() == 3 && version.split('.').all(|p| p.parse::<u32>().is_ok()),
        "version в {} должна быть вида X.Y.Z, получено {version:?}",
        manifest.display()
    );
    println!("cargo:rustc-env=ZAM_QDRANT_PINNED_VERSION={version}");
}

/// Значение `version = "..."` до первой таблицы `[...]`.
fn top_level_version(text: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            return None;
        }
        if let Some(rest) = line.strip_prefix("version") {
            let value = rest.trim_start().strip_prefix('=')?.trim();
            let value = value.split('#').next()?.trim();
            return Some(value.trim_matches('"').to_string());
        }
    }
    None
}
