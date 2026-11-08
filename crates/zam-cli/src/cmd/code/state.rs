//! Локальное состояние карты кода (спецификация M5.6 §4, §5; решения составителя плана 6, 7):
//!
//! - `cache/code-state.json` — `{server, solution, files: {путь: {fp, size, hash, sent_at}},
//!   crashed: [[путь, hash]], extractor, recv_count}`: по отпечатку `fp` хэш берётся без чтения файла; `crashed` —
//!   файлы, на которых оборвался разбор (пропуск `parser_crash`, пока хэш тот же);
//! - `cache/code-sync.json` — `{server, solution, last_ok_at, head}`: малый файл для хуков
//!   (задача 6 — «не чаще `auto_interval_min` или при смене `HEAD`»).
//!
//! Запись — атомарно, права 0600 ([`cache::write_atomic`], как кэши M4). Битый или чужой
//! формат — пустое состояние (файлы перечитаются, загрузок от этого не прибавится: решает `need`
//! сервера).
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::cmd::hook::cache;

/// Предел `code-state.json`, байт (50 000 файлов × ≈ 200 байт с запасом).
const STATE_MAX: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeState {
    #[serde(default)]
    pub server: String,
    #[serde(default)]
    pub solution: String,
    #[serde(default)]
    pub files: BTreeMap<String, FileState>,
    #[serde(default)]
    pub crashed: Vec<(String, String)>,
    /// [`zam_code::EXTRACT_VERSION`], которой разобраны файлы; другая — полная перезагрузка
    /// (M6, версия 3: `recv_count` — все файлы решения перечитываются при первой синхронизации).
    /// Нет поля (состояние до версии 2) — 0.
    #[serde(default)]
    pub extractor: u32,
    /// Файлы загружены серверу, который принимает `refs[].recv_count` (ответ `POST /code/sync`,
    /// M6 §12а). Сервер стал его принимать (обновился) — полная перезагрузка: иначе вызовы по
    /// получателю попали бы в карту только с правкой файла. Нет поля — `false`.
    #[serde(default)]
    pub recv_count: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileState {
    /// `oid:<sha>` (чистый файл git) или `mt:<размер>:<mtime_ns>`.
    pub fp: String,
    pub size: u64,
    /// blake3 содержимого, 64 hex.
    pub hash: String,
    /// Когда файл последний раз загружен этим клиентом (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_at: Option<String>,
}

/// Отметка последней успешной синхронизации (для хуков).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncMark {
    pub server: String,
    pub solution: String,
    pub last_ok_at: String,
    #[serde(default)]
    pub head: Option<String>,
}

pub fn cache_dir(root: &Path) -> PathBuf {
    root.join(".zeroaimemory").join("cache")
}

pub fn state_path(root: &Path) -> PathBuf {
    cache_dir(root).join("code-state.json")
}

pub fn mark_path(root: &Path) -> PathBuf {
    cache_dir(root).join("code-sync.json")
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    let m = std::fs::metadata(path).ok()?;
    if !m.is_file() || m.len() > STATE_MAX {
        return None;
    }
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

impl CodeState {
    /// Состояние проекта; нет или битое — пустое.
    pub fn load(root: &Path) -> CodeState {
        read_json(&state_path(root)).unwrap_or_default()
    }

    pub fn save(&self, root: &Path) -> std::io::Result<()> {
        let bytes = serde_json::to_vec(self).map_err(std::io::Error::other)?;
        cache::write_atomic(&state_path(root), &bytes)
    }

    /// Файл из журнала падений с этим хэшем.
    pub fn is_crashed(&self, path: &str, hash: &str) -> bool {
        self.crashed.iter().any(|(p, h)| p == path && h == hash)
    }
}

impl SyncMark {
    pub fn load(root: &Path) -> Option<SyncMark> {
        read_json(&mark_path(root))
    }

    pub fn save(&self, root: &Path) -> std::io::Result<()> {
        let bytes = serde_json::to_vec(self).map_err(std::io::Error::other)?;
        cache::write_atomic(&mark_path(root), &bytes)
    }
}
