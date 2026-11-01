//! Общие помощники интеграционных тестов.
#![allow(dead_code)]
use std::path::PathBuf;

pub mod api;
pub mod ml;
pub mod process;
pub mod qdrant;

use zam_server::config::ServerConfig;

/// Уникальный (по имени) пустой каталог внутри target.
pub fn tmp(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("boot-{name}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Конфигурация теста в каталоге `dir`. Рабочий процесс разбора документов — бинарник пакета:
/// текущий исполняемый файл теста подкоманды `doc-worker` не знает.
pub fn cfg_in(dir: PathBuf) -> ServerConfig {
    let mut c = ServerConfig {
        data_dir: dir,
        ..ServerConfig::default()
    };
    c.docs.worker_program = Some(PathBuf::from(env!("CARGO_BIN_EXE_zam-server")));
    c
}
