//! Опись копии `manifest.json` (M6 §12б.2) и проверка путей описи.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::BackupError;

/// Версия формата описи.
pub const MANIFEST_FORMAT: u32 = 1;
/// Имя файла описи в корне копии.
pub const MANIFEST_FILE: &str = "manifest.json";

/// Файл копии: путь от корня копии (через `/`), размер и SHA-256.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

/// Отпечатки нарезки базы пользователя: `meta.chunker`, `meta.chunker_entries`,
/// `meta.chunker_attachments` (на момент снимка).
pub type Chunkers = zam_store::Chunkers;

/// Опись копии.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    pub server_version: String,
    /// UTC, RFC 3339.
    pub created_at: String,
    /// Имя машины-источника (справочно).
    pub source_machine: Option<String>,
    /// `{файл: user_version}`.
    pub schemas: BTreeMap<String, u32>,
    pub files: Vec<FileEntry>,
    /// `model_id` векторов кэша по базам пользователей.
    pub embedding_models: BTreeMap<String, Vec<String>>,
    pub chunkers: BTreeMap<String, Chunkers>,
    /// Расхождения снимков баз (каждая база — свой снимок; §12б.2).
    pub drift: Vec<String>,
    /// Всегда `false`: мастер-ключ Vault в копию не входит (ТЗ §7.6).
    pub master_key_included: bool,
    /// Копия не прошла: файлы блобов, которых нет (`<user>/<sha256>`); у полной копии пусто.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_blobs: Vec<String>,
}

/// Имя машины: `COMPUTERNAME`/`HOSTNAME`, на Linux без них — `/etc/hostname`.
pub fn machine_name() -> Option<String> {
    for k in ["COMPUTERNAME", "HOSTNAME"] {
        if let Ok(v) = std::env::var(k)
            && !v.trim().is_empty()
        {
            return Some(v.trim().to_string());
        }
    }
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// SHA-256 файла (hex) и его размер.
pub fn sha256_file(path: &Path) -> std::io::Result<(String, u64)> {
    let mut f = std::fs::File::open(path)?;
    sha256_reader(&mut f)
}

/// SHA-256 потока (hex) и число байт.
pub fn sha256_reader(r: &mut impl Read) -> std::io::Result<(String, u64)> {
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut n = 0u64;
    loop {
        let k = r.read(&mut buf)?;
        if k == 0 {
            break;
        }
        h.update(&buf[..k]);
        n += k as u64;
    }
    Ok((hex(&h.finalize()), n))
}

pub(crate) fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Вид файла копии по пути описи.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    SystemDb,
    UserDb(zam_core::Id),
    Blob { user: zam_core::Id, sha: String },
}

/// Разбирает путь описи: только `system.db`, `users/<id>.db` и
/// `blobs/<id>/<aa>/<sha256>` — без `..`, абсолютных путей и лишних уровней (опись чужого
/// архива не пишет мимо каталога данных).
pub fn classify(path: &str) -> Result<Kind, BackupError> {
    let bad = || BackupError::BadArchive(format!("недопустимый путь в описи: {path}"));
    let parts: Vec<&str> = path.split('/').collect();
    match parts.as_slice() {
        ["system.db"] => Ok(Kind::SystemDb),
        ["users", f] => {
            let id = f.strip_suffix(".db").ok_or_else(bad)?;
            Ok(Kind::UserDb(id.parse().map_err(|_| bad())?))
        }
        ["blobs", u, aa, sha] => {
            let user = u.parse().map_err(|_| bad())?;
            if !zam_store::blobs::is_sha256(sha) || sha.get(..2) != Some(*aa) {
                return Err(bad());
            }
            Ok(Kind::Blob {
                user,
                sha: (*sha).to_string(),
            })
        }
        _ => Err(bad()),
    }
}

/// Путь файла описи под корнем `root`.
pub fn local(root: &Path, path: &str) -> PathBuf {
    path.split('/').fold(root.to_path_buf(), |p, s| p.join(s))
}

impl Manifest {
    /// Читает опись из `root/manifest.json`.
    pub fn read(root: &Path) -> Result<Manifest, BackupError> {
        let text = std::fs::read_to_string(root.join(MANIFEST_FILE))
            .map_err(|e| BackupError::BadArchive(format!("нет описи {MANIFEST_FILE}: {e}")))?;
        Self::parse(&text)
    }

    /// Разбор и проверка формата описи.
    pub fn parse(text: &str) -> Result<Manifest, BackupError> {
        let m: Manifest = serde_json::from_str(text)
            .map_err(|e| BackupError::BadArchive(format!("опись не разбирается: {e}")))?;
        if m.format != MANIFEST_FORMAT {
            return Err(BackupError::BadArchive(format!(
                "формат описи {} не поддерживается (нужен {MANIFEST_FORMAT})",
                m.format
            )));
        }
        if !m.missing_blobs.is_empty() {
            return Err(BackupError::BadArchive(format!(
                "копия неполна: нет блобов {}",
                m.missing_blobs.len()
            )));
        }
        Ok(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_rejects_escapes() {
        let id = zam_core::Id::new();
        let sha = "ab".to_string() + &"0".repeat(62);
        assert_eq!(classify("system.db").unwrap(), Kind::SystemDb);
        assert_eq!(
            classify(&format!("users/{id}.db")).unwrap(),
            Kind::UserDb(id)
        );
        assert!(matches!(
            classify(&format!("blobs/{id}/ab/{sha}")).unwrap(),
            Kind::Blob { .. }
        ));
        for p in [
            "../system.db",
            "/system.db",
            "users/../system.db",
            "users/x.db",
            &format!("blobs/{id}/cd/{sha}"),
            &format!("blobs/{id}/ab/../{sha}"),
            "master.key",
            "qdrant/storage/x",
        ] {
            assert!(classify(p).is_err(), "{p}");
        }
    }
}
