//! Манифесты зависимостей поставки (`deploy/*.toml`).

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// Платформы поставки.
pub const PLATFORMS: [&str; 2] = ["windows-x64", "linux-x64"];

/// Манифесты, входящие в поставку (имена файлов в `deploy/`).
pub const MANIFESTS: [&str; 3] = ["qdrant", "onnxruntime", "pdfium"];

#[derive(Debug, Deserialize)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub platform: BTreeMap<String, PlatformEntry>,
}

#[derive(Debug, Deserialize)]
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

impl Manifest {
    pub fn parse(text: &str) -> Result<Self> {
        toml::from_str(text).context("не удалось разобрать манифест")
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("не удалось прочитать {}", path.display()))?;
        Self::parse(&text).with_context(|| path.display().to_string())
    }

    /// Проверка манифеста; `allow_empty_hash` — только для режима `--print-hashes`.
    pub fn validate(&self, allow_empty_hash: bool) -> Result<()> {
        for (plat, e) in &self.platform {
            let ctx = || format!("{} [{plat}]", self.name);
            if !e.url.starts_with("https://") {
                bail!("{}: url должен начинаться с https://", ctx());
            }
            let h = e.sha256.trim();
            if h.is_empty() {
                if !allow_empty_hash {
                    bail!("{}: sha256 не задан; используйте --print-hashes", ctx());
                }
            } else if h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
                bail!("{}: sha256 должен быть 64 hex-символа", ctx());
            }
            crate::fetch::dest_names(e).with_context(ctx)?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn platforms(&self) -> Vec<&str> {
        self.platform.keys().map(String::as_str).collect()
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
    fn manifest_parses_and_lists_platforms() {
        let m = Manifest::parse(SAMPLE).unwrap();
        assert_eq!(m.name, "qdrant");
        assert_eq!(m.version, "1.19.2");
        assert_eq!(m.platforms(), vec!["linux-x64", "windows-x64"]);
        let win = &m.platform["windows-x64"];
        assert_eq!(win.sha256, "abc");
        assert_eq!(win.extract, vec!["qdrant.exe"]);
        let lin = &m.platform["linux-x64"];
        assert_eq!(lin.sha256, "");
        assert_eq!(lin.rename["qdrant"], "qdrant-bin");
    }

    #[test]
    fn real_manifests_are_valid() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../deploy");
        for name in MANIFESTS {
            let m = Manifest::load(&dir.join(format!("{name}.toml"))).unwrap();
            assert_eq!(m.name, name);
            m.validate(false).unwrap();
            for p in PLATFORMS {
                assert!(m.platform.contains_key(p), "{name}: нет платформы {p}");
            }
        }
    }

    #[test]
    fn validation_rejects_bad_hash_and_url() {
        let ok = SAMPLE.replace("abc", &"A".repeat(64));
        let m = Manifest::parse(&ok).unwrap();
        assert!(m.validate(false).is_err()); // у linux-x64 хэш пуст
        assert!(m.validate(true).is_ok());
        assert!(Manifest::parse(SAMPLE).unwrap().validate(true).is_err()); // "abc"
        let http = ok.replace(
            "https://example.invalid/q.zip",
            "http://example.invalid/q.zip",
        );
        assert!(Manifest::parse(&http).unwrap().validate(true).is_err());
    }
}
