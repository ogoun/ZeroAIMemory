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

pub fn cfg_in(dir: PathBuf) -> ServerConfig {
    ServerConfig {
        data_dir: dir,
        ..ServerConfig::default()
    }
}
