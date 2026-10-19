//! Тестовые помощники: файлы пишутся только внутри `target/`.
use std::path::PathBuf;

/// Записывает файл в `<repo>/target/test-tmp/zam-server/<name>` и возвращает путь.
pub fn write_in_target(name: &str, content: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-tmp/zam-server");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, content).unwrap();
    path
}

/// Пустой каталог `<repo>/target/test-tmp/zam-server/<name>` (пересоздаётся).
pub fn dir_in_target(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/test-tmp/zam-server")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
