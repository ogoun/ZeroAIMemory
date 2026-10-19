//! Манифесты зависимостей поставки (`deploy/*.toml`).

// Манифесты и их проверка — общие с сервером (`zam-server qdrant fetch`, `models fetch`,
// `cuda fetch`): один разбор `deploy/*.toml` в крейте zam-fetch.
pub use zam_fetch::{CudaManifest, DepManifest as Manifest, ModelsManifest, PLATFORMS};

/// Манифесты, входящие в поставку (имена файлов в `deploy/`).
pub const MANIFESTS: [&str; 4] = ["qdrant", "onnxruntime", "onnxruntime-gpu", "pdfium"];

/// Манифест моделей (`deploy/models.toml`).
pub const MODELS_MANIFEST: &str = "models.toml";

/// Манифест библиотек NVIDIA для CUDA EP (`deploy/cuda.toml`).
pub const CUDA_MANIFEST: &str = "cuda.toml";

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

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
        let plats: Vec<&str> = m.platform.keys().map(String::as_str).collect();
        assert_eq!(plats, vec!["linux-x64", "windows-x64"]);
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
        assert!(MANIFESTS.contains(&"onnxruntime-gpu"));
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
