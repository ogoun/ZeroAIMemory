//! Манифест зависимости поставки (`deploy/qdrant.toml`, `onnxruntime*.toml`, `pdfium.toml`).

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use crate::FetchError;

/// Платформы поставки.
pub const PLATFORMS: [&str; 2] = ["windows-x64", "linux-x64"];

/// Платформа текущей сборки в терминах манифестов.
pub fn host_platform() -> &'static str {
    if cfg!(windows) {
        "windows-x64"
    } else {
        "linux-x64"
    }
}

/// Манифест: имя, закреплённая версия и архив для каждой платформы.
#[derive(Debug, Clone, Deserialize)]
pub struct DepManifest {
    pub name: String,
    pub version: String,
    pub platform: BTreeMap<String, PlatformEntry>,
}

/// Архив платформы.
#[derive(Debug, Clone, Deserialize)]
pub struct PlatformEntry {
    pub url: String,
    /// Пустая строка — хэш ещё не закреплён.
    #[serde(default)]
    pub sha256: String,
    /// Пути внутри архива (с корневым каталогом архива, если он есть).
    pub extract: Vec<String>,
    /// Необязательное переименование при извлечении: путь в архиве → имя файла.
    #[serde(default)]
    pub rename: BTreeMap<String, String>,
}

impl DepManifest {
    pub fn parse(text: &str) -> Result<Self, FetchError> {
        toml::from_str(text)
            .map_err(|e| FetchError::Invalid(format!("не удалось разобрать манифест: {e}")))
    }

    pub fn load(path: &Path) -> Result<Self, FetchError> {
        let text = std::fs::read_to_string(path).map_err(FetchError::io(format!(
            "не удалось прочитать {}",
            path.display()
        )))?;
        Self::parse(&text).map_err(|e| FetchError::Invalid(format!("{}: {e}", path.display())))
    }

    /// Архив платформы.
    pub fn entry(&self, platform: &str) -> Result<&PlatformEntry, FetchError> {
        self.platform.get(platform).ok_or_else(|| {
            FetchError::Invalid(format!(
                "{}: нет платформы {platform} в манифесте",
                self.name
            ))
        })
    }

    /// Проверка манифеста; `allow_empty_hash` — только для режима `--print-hashes`.
    pub fn validate(&self, allow_empty_hash: bool) -> Result<(), FetchError> {
        for (plat, e) in &self.platform {
            let ctx = format!("{} [{plat}]", self.name);
            if !e.url.starts_with("https://") {
                return Err(FetchError::Invalid(format!(
                    "{ctx}: url должен начинаться с https://"
                )));
            }
            let h = e.sha256.trim();
            if h.is_empty() {
                if !allow_empty_hash {
                    return Err(FetchError::Invalid(format!(
                        "{ctx}: sha256 не задан; используйте --print-hashes"
                    )));
                }
            } else if h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(FetchError::Invalid(format!(
                    "{ctx}: sha256 должен быть 64 hex-символа"
                )));
            }
            crate::dest_names(e).map_err(|err| FetchError::Invalid(format!("{ctx}: {err}")))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
name = "qdrant"
version = "1.19.2"
[platform.windows-x64]
url = "https://example.invalid/q.zip"
sha256 = "abc"
extract = ["qdrant.exe"]
[platform.linux-x64]
url = "https://example.invalid/q.tar.gz"
extract = ["qdrant"]
rename = { "qdrant" = "qdrant-bin" }
"#;

    #[test]
    fn manifest_parses() {
        let m = DepManifest::parse(SAMPLE).unwrap();
        assert_eq!((m.name.as_str(), m.version.as_str()), ("qdrant", "1.19.2"));
        let keys: Vec<&str> = m.platform.keys().map(String::as_str).collect();
        assert_eq!(keys, ["linux-x64", "windows-x64"]);
        assert_eq!(m.entry("windows-x64").unwrap().sha256, "abc");
        assert_eq!(m.entry("linux-x64").unwrap().rename["qdrant"], "qdrant-bin");
        assert!(m.entry("mac").is_err());
    }

    #[test]
    fn validation_rejects_bad_hash_and_url() {
        let ok = SAMPLE.replace("abc", &"A".repeat(64));
        let m = DepManifest::parse(&ok).unwrap();
        assert!(m.validate(false).is_err()); // у linux-x64 хэш пуст
        assert!(m.validate(true).is_ok());
        assert!(DepManifest::parse(SAMPLE).unwrap().validate(true).is_err()); // "abc"
        let http = ok.replace(
            "https://example.invalid/q.zip",
            "http://example.invalid/q.zip",
        );
        assert!(DepManifest::parse(&http).unwrap().validate(true).is_err());
    }

    #[test]
    fn real_qdrant_manifest_is_valid() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("deploy")
            .join("qdrant.toml");
        let m = DepManifest::load(&p).unwrap();
        m.validate(false).unwrap();
        for plat in PLATFORMS {
            m.entry(plat).unwrap();
        }
        assert!(PLATFORMS.contains(&host_platform()));
    }
}
