//! Манифесты зависимостей поставки (`deploy/*.toml`).

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// Платформы поставки.
pub const PLATFORMS: [&str; 2] = ["windows-x64", "linux-x64"];

/// Манифесты, входящие в поставку (имена файлов в `deploy/`).
pub const MANIFESTS: [&str; 4] = ["qdrant", "onnxruntime", "onnxruntime-gpu", "pdfium"];

/// Манифест моделей (`deploy/models.toml`).
pub const MODELS_MANIFEST: &str = "models.toml";

/// Манифест библиотек NVIDIA для CUDA EP (`deploy/cuda.toml`).
pub const CUDA_MANIFEST: &str = "cuda.toml";

/// Источник redist-архивов NVIDIA.
const NVIDIA_REDIST: &str = "https://developer.download.nvidia.com/compute/";

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

/// Проверка, что `h` — 64 hex-символа (SHA-256).
fn check_sha256(h: &str) -> Result<()> {
    if h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("sha256 должен быть 64 hex-символа, получено '{h}'");
    }
    Ok(())
}

/// Простое имя файла или каталога: без разделителей, `..` и управляющих символов.
fn check_plain_name(kind: &str, name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
    if !ok {
        bail!("{kind} '{name}' должен быть простым именем (латиница, цифры, '-', '_', '.')");
    }
    Ok(())
}

/// Ссылка на Hugging Face обязана быть закреплена коммитом: `/resolve/<40 hex>/`.
fn check_hf_revision(url: &str) -> Result<()> {
    let Some(rest) = url.strip_prefix("https://huggingface.co/") else {
        return Ok(());
    };
    let rev = rest
        .split('/')
        .skip_while(|s| *s != "resolve")
        .nth(1)
        .unwrap_or("");
    if rev.len() != 40 || !rev.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("{url}: ревизия Hugging Face должна быть хэшем коммита (/resolve/<40 hex>/)");
    }
    Ok(())
}

/// Назначение модели.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelRole {
    Embed,
    Rerank,
    Test,
}

/// Устройство, для которого предназначен вариант модели.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelDevice {
    Cpu,
    Gpu,
    Any,
}

/// Пулинг выхода `last_hidden_state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Pooling {
    Cls,
    Mean,
}

/// Манифест моделей.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelsManifest {
    #[serde(default)]
    pub model: Vec<ModelEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelEntry {
    /// Каталог `<models_dir>/<id>/`.
    pub id: String,
    pub role: ModelRole,
    pub device: ModelDevice,
    /// SPDX-идентификатор лицензии весов.
    pub license: String,
    /// Входит ли модель в архив `zam-models`.
    pub bundle: bool,
    /// Загрузка только с явным согласием с лицензией (`--accept-nc-license`).
    #[serde(default)]
    pub requires_license_ack: bool,
    pub files: Vec<ModelFile>,
    pub spec: ModelSpec,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelFile {
    pub url: String,
    /// Имя файла в каталоге модели.
    pub path: String,
    /// Размер в байтах (для проверки места и прогресса).
    pub size: u64,
    pub sha256: String,
}

/// Описатель модели для zam-ml.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSpec {
    /// Размерность dense-вектора (embed, test).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dim: Option<usize>,
    /// Пулинг (embed, test).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pooling: Option<Pooling>,
    /// Файл sparse-головы из `files`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sparse_head: Option<String>,
    pub max_tokens_passage: usize,
    pub max_tokens_query: usize,
    /// Активации квантуются динамически по всему тензору (`DynamicQuantizeLinear`):
    /// соседи по батчу меняют вектор, поэтому zam-ml прогоняет такие модели по одному тексту.
    #[serde(default, skip_serializing_if = "is_false")]
    pub dynamic_quant: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Содержимое `<models_dir>/<id>/model.toml`, которое читает `zam_ml::ModelSpec::load`.
#[derive(Serialize)]
struct ModelSpecFile<'a> {
    id: &'a str,
    #[serde(flatten)]
    spec: &'a ModelSpec,
}

/// Имя описателя модели в её каталоге.
pub const MODEL_SPEC_FILE: &str = "model.toml";

impl ModelEntry {
    /// `model.toml` для каталога модели: `id` и поля `[model.spec]` верхнего уровня.
    pub fn spec_toml(&self) -> Result<String> {
        let body = toml::to_string(&ModelSpecFile {
            id: &self.id,
            spec: &self.spec,
        })
        .context("не удалось сериализовать описатель модели")?;
        Ok(format!(
            "# Описатель модели для zam-ml; создан `cargo xtask fetch-models` из deploy/models.toml
{body}"
        ))
    }
}

impl ModelsManifest {
    pub fn parse(text: &str) -> Result<Self> {
        toml::from_str(text).context("не удалось разобрать манифест моделей")
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("не удалось прочитать {}", path.display()))?;
        Self::parse(&text).with_context(|| path.display().to_string())
    }

    pub fn validate(&self) -> Result<()> {
        if self.model.is_empty() {
            bail!("в манифесте нет ни одной записи [[model]]");
        }
        let mut ids: Vec<&str> = Vec::new();
        for m in &self.model {
            m.validate().with_context(|| format!("модель '{}'", m.id))?;
            if ids.contains(&m.id.as_str()) {
                bail!("модель '{}' описана дважды", m.id);
            }
            ids.push(&m.id);
        }
        Ok(())
    }

    /// Выбор моделей для загрузки: явные `ids` плюс тестовые при `test`;
    /// без того и другого — комплект `zam-models` (`bundle = true`).
    /// Модели с `requires_license_ack` — только при `accept_nc`.
    pub fn select(&self, ids: &[String], test: bool, accept_nc: bool) -> Result<Vec<&ModelEntry>> {
        let mut out: Vec<&ModelEntry> = Vec::new();
        for id in ids {
            let m = self.model.iter().find(|m| &m.id == id).with_context(|| {
                let known: Vec<&str> = self.model.iter().map(|m| m.id.as_str()).collect();
                format!("неизвестная модель '{id}', есть: {}", known.join(", "))
            })?;
            if !out.iter().any(|x| x.id == m.id) {
                out.push(m);
            }
        }
        if test {
            for m in self.model.iter().filter(|m| m.role == ModelRole::Test) {
                if !out.iter().any(|x| x.id == m.id) {
                    out.push(m);
                }
            }
        }
        if ids.is_empty() && !test {
            out = self.model.iter().filter(|m| m.bundle).collect();
        }
        if !accept_nc && let Some(m) = out.iter().find(|m| m.requires_license_ack) {
            bail!(
                "модель '{}' распространяется по лицензии {} (только некоммерческое использование); \
                 для загрузки подтвердите согласие флагом --accept-nc-license",
                m.id,
                m.license
            );
        }
        Ok(out)
    }
}

impl ModelEntry {
    fn validate(&self) -> Result<()> {
        check_plain_name("id", &self.id)?;
        if self.license.trim().is_empty() {
            bail!("не задана license");
        }
        if self.requires_license_ack && self.bundle {
            bail!("requires_license_ack = true допустимо только при bundle = false");
        }
        if self.files.is_empty() {
            bail!("пустой список files");
        }
        let mut paths: Vec<&str> = Vec::new();
        for f in &self.files {
            let ctx = || format!("файл '{}'", f.path);
            check_plain_name("path", &f.path).with_context(ctx)?;
            if !f.url.starts_with("https://") {
                bail!("{}: url должен начинаться с https://", ctx());
            }
            check_hf_revision(&f.url).with_context(ctx)?;
            check_sha256(f.sha256.trim()).with_context(ctx)?;
            if f.size == 0 {
                bail!("{}: size должен быть больше нуля", ctx());
            }
            if paths.contains(&f.path.as_str()) {
                bail!("два файла с одним path '{}'", f.path);
            }
            paths.push(&f.path);
        }
        for need in ["model.onnx", "tokenizer.json"] {
            if !paths.contains(&need) {
                bail!("в files нет обязательного файла {need}");
            }
        }
        let s = &self.spec;
        if let Some(h) = &s.sparse_head
            && !paths.contains(&h.as_str())
        {
            bail!("sparse_head '{h}' не найден среди files");
        }
        if s.max_tokens_passage == 0 || s.max_tokens_query == 0 {
            bail!("max_tokens_passage и max_tokens_query должны быть больше нуля");
        }
        match self.role {
            ModelRole::Embed | ModelRole::Test => {
                if s.dim.is_none_or(|d| d == 0) {
                    bail!("для эмбеддера нужен spec.dim > 0");
                }
                if s.pooling.is_none() {
                    bail!("для эмбеддера нужен spec.pooling (cls | mean)");
                }
            }
            ModelRole::Rerank => {
                if s.dim.is_some() || s.pooling.is_some() || s.sparse_head.is_some() {
                    bail!("у реранкера не бывает spec.dim, spec.pooling и spec.sparse_head");
                }
            }
        }
        Ok(())
    }
}

/// Манифест библиотек NVIDIA (CUDA и cuDNN) для CUDA EP.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CudaManifest {
    /// Выпуск CUDA redist (`redistrib_<cuda>.json`).
    pub cuda: String,
    /// Выпуск cuDNN redist (`redistrib_<cudnn>.json`).
    pub cudnn: String,
    pub lib: Vec<CudaLib>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CudaLib {
    pub platform: String,
    /// Компонент redist (`cuda_cudart`, `libcublas`, `cudnn`, …).
    pub component: String,
    pub version: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,
    /// Пути внутри архива; файл кладётся в `<exe_dir>/cuda/` под последним компонентом пути.
    pub extract: Vec<String>,
}

impl CudaManifest {
    pub fn parse(text: &str) -> Result<Self> {
        toml::from_str(text).context("не удалось разобрать манифест CUDA")
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("не удалось прочитать {}", path.display()))?;
        Self::parse(&text).with_context(|| path.display().to_string())
    }

    pub fn validate(&self) -> Result<()> {
        if self.lib.is_empty() {
            bail!("в манифесте CUDA нет ни одной записи [[lib]]");
        }
        for l in &self.lib {
            let ctx = || format!("{} {} [{}]", l.component, l.version, l.platform);
            if !PLATFORMS.contains(&l.platform.as_str()) {
                bail!(
                    "{}: неизвестная платформа '{}', допустимы: {}",
                    ctx(),
                    l.platform,
                    PLATFORMS.join(", ")
                );
            }
            if !l.url.starts_with("https://") {
                bail!("{}: url должен начинаться с https://", ctx());
            }
            if !l.url.starts_with(NVIDIA_REDIST) {
                bail!("{}: url должен вести на {NVIDIA_REDIST}", ctx());
            }
            check_sha256(l.sha256.trim()).with_context(ctx)?;
            if l.extract.is_empty() {
                bail!("{}: пустой список extract", ctx());
            }
        }
        for p in PLATFORMS {
            self.dest_names(p)?;
        }
        Ok(())
    }

    /// Имена файлов в `<exe_dir>/cuda/` для платформы; дубликаты и пустые имена — ошибка.
    pub fn dest_names(&self, platform: &str) -> Result<Vec<String>> {
        let mut names: Vec<String> = Vec::new();
        for l in self.lib.iter().filter(|l| l.platform == platform) {
            for p in &l.extract {
                let name = p.rsplit(['/', '\\']).next().unwrap_or("");
                check_plain_name("файл", name)
                    .with_context(|| format!("{} [{platform}]: '{p}'", l.component))?;
                if names.iter().any(|n| n == name) {
                    bail!("[{platform}]: два файла извлекаются под одним именем '{name}'");
                }
                names.push(name.to_string());
            }
        }
        Ok(names)
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

    // ---- deploy/models.toml ----

    fn hf(file: &str) -> String {
        format!(
            "https://huggingface.co/o/r/resolve/0123456789abcdef0123456789abcdef01234567/{file}"
        )
    }

    fn models_sample() -> String {
        format!(
            r#"
[[model]]
id = "m1"
role = "embed"
device = "cpu"
license = "MIT"
bundle = true
files = [
    {{ url = "{}", path = "model.onnx", size = 3, sha256 = "{}" }},
    {{ url = "{}", path = "tokenizer.json", size = 3, sha256 = "{}" }},
    {{ url = "{}", path = "head.pt", size = 3, sha256 = "{}" }},
]
[model.spec]
dim = 8
pooling = "cls"
sparse_head = "head.pt"
dynamic_quant = true
max_tokens_passage = 16
max_tokens_query = 8

[[model]]
id = "t1"
role = "test"
device = "any"
license = "Apache-2.0"
bundle = false
files = [
    {{ url = "{}", path = "model.onnx", size = 3, sha256 = "{}" }},
    {{ url = "{}", path = "tokenizer.json", size = 3, sha256 = "{}" }},
]
[model.spec]
dim = 4
pooling = "mean"
max_tokens_passage = 16
max_tokens_query = 8

[[model]]
id = "nc"
role = "rerank"
device = "cpu"
license = "CC-BY-NC-4.0"
bundle = false
requires_license_ack = true
files = [
    {{ url = "{}", path = "model.onnx", size = 3, sha256 = "{}" }},
    {{ url = "{}", path = "tokenizer.json", size = 3, sha256 = "{}" }},
]
[model.spec]
max_tokens_passage = 16
max_tokens_query = 8
"#,
            hf("onnx/model_int8.onnx"),
            "a".repeat(64),
            hf("tokenizer.json"),
            "b".repeat(64),
            hf("head.pt"),
            "c".repeat(64),
            hf("t/model.onnx"),
            "d".repeat(64),
            hf("t/tokenizer.json"),
            "e".repeat(64),
            hf("nc/model.onnx"),
            "f".repeat(64),
            hf("nc/tokenizer.json"),
            "0".repeat(64),
        )
    }

    /// Подмена `from` → `to` в образце делает манифест недопустимым,
    /// а текст ошибки содержит `needle`.
    fn models_rejects(from: &str, to: &str, needle: &str) {
        let text = models_sample();
        assert!(text.contains(from), "в образце нет '{from}'");
        let bad = text.replacen(from, to, 1);
        let err = match ModelsManifest::parse(&bad) {
            Ok(m) => format!(
                "{:#}",
                m.validate()
                    .expect_err(&format!("ожидалась ошибка: {from} -> {to}"))
            ),
            Err(e) => format!("{e:#}"),
        };
        assert!(err.contains(needle), "'{from}' -> '{to}': {err}");
    }

    #[test]
    fn models_manifest_parses() {
        let m = ModelsManifest::parse(&models_sample()).unwrap();
        m.validate().unwrap();
        assert_eq!(m.model.len(), 3);
        let e = &m.model[0];
        assert_eq!(e.id, "m1");
        assert_eq!(e.role, ModelRole::Embed);
        assert_eq!(e.device, ModelDevice::Cpu);
        assert!(e.bundle && !e.requires_license_ack);
        assert_eq!(e.files[0].path, "model.onnx");
        assert_eq!(e.files[0].size, 3);
        assert_eq!(e.spec.dim, Some(8));
        assert_eq!(e.spec.pooling, Some(Pooling::Cls));
        assert_eq!(e.spec.sparse_head.as_deref(), Some("head.pt"));
        assert_eq!(
            (e.spec.max_tokens_passage, e.spec.max_tokens_query),
            (16, 8)
        );
        assert_eq!(m.model[1].spec.pooling, Some(Pooling::Mean));
        assert!(m.model[2].requires_license_ack);
    }

    #[test]
    fn models_validation_rejects_bad_entries() {
        // каждый файл — https и 64-символьный sha256
        models_rejects(
            "https://huggingface.co",
            "http://huggingface.co",
            "https://",
        );
        models_rejects(&"a".repeat(64), "abc", "sha256");
        models_rejects(&"b".repeat(64), &"z".repeat(64), "sha256");
        // ревизия Hugging Face закреплена коммитом, а не веткой
        models_rejects(
            "resolve/0123456789abcdef0123456789abcdef01234567/onnx",
            "resolve/main/onnx",
            "ревизи",
        );
        // согласие с лицензией допустимо только вне zam-models
        models_rejects(
            "bundle = false\nrequires_license_ack = true",
            "bundle = true\nrequires_license_ack = true",
            "requires_license_ack",
        );
        models_rejects("id = \"t1\"", "id = \"m1\"", "m1");
        models_rejects("id = \"t1\"", "id = \"../x\"", "../x");
        models_rejects("path = \"head.pt\"", "path = \"../head.pt\"", "../head.pt");
        models_rejects(
            "path = \"head.pt\"",
            "path = \"tokenizer.json\"",
            "tokenizer.json",
        );
        models_rejects(
            "sparse_head = \"head.pt\"",
            "sparse_head = \"nope.pt\"",
            "nope.pt",
        );
        models_rejects("dim = 8\n", "", "dim");
        models_rejects("pooling = \"mean\"\n", "", "pooling");
        models_rejects("pooling = \"cls\"", "pooling = \"max\"", "max");
        models_rejects("role = \"test\"", "role = \"judge\"", "judge");
        models_rejects(
            "path = \"model.onnx\", size = 3, sha256 = \"dddd",
            "path = \"m.onnx\", size = 3, sha256 = \"dddd",
            "model.onnx",
        );
        models_rejects(
            "max_tokens_query = 8\n\n[[model]]\nid = \"t1\"",
            "max_tokens_query = 0\n\n[[model]]\nid = \"t1\"",
            "max_tokens",
        );
    }

    #[test]
    fn models_validation_rejects_empty_files_and_manifest() {
        let text = "[[model]]\nid = \"x\"\nrole = \"test\"\ndevice = \"any\"\nlicense = \"MIT\"\n\
                    bundle = false\nfiles = []\n[model.spec]\ndim = 4\npooling = \"mean\"\n\
                    max_tokens_passage = 4\nmax_tokens_query = 4\n";
        let err = format!(
            "{:#}",
            ModelsManifest::parse(text).unwrap().validate().unwrap_err()
        );
        assert!(err.contains("files"), "{err}");
        let err = format!(
            "{:#}",
            ModelsManifest::parse("").unwrap().validate().unwrap_err()
        );
        assert!(err.contains("model"), "{err}");
    }

    #[test]
    fn models_selection() {
        let m = ModelsManifest::parse(&models_sample()).unwrap();
        let ids = |sel: Vec<&ModelEntry>| sel.iter().map(|e| e.id.clone()).collect::<Vec<_>>();
        // по умолчанию — комплект zam-models
        assert_eq!(ids(m.select(&[], false, false).unwrap()), ["m1"]);
        // --test — только тестовые модели
        assert_eq!(ids(m.select(&[], true, false).unwrap()), ["t1"]);
        assert_eq!(
            ids(m.select(&["m1".into()], true, false).unwrap()),
            ["m1", "t1"]
        );
        let err = m.select(&["zz".into()], false, false).unwrap_err();
        assert!(err.to_string().contains("zz"), "{err}");
        // модель с некоммерческой лицензией — только с явным согласием
        let err = m
            .select(&["nc".into()], false, false)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("--accept-nc-license") && err.contains("CC-BY-NC-4.0"),
            "{err}"
        );
        assert_eq!(ids(m.select(&["nc".into()], false, true).unwrap()), ["nc"]);
    }

    #[test]
    fn real_models_manifest_is_valid() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("deploy");
        let m = ModelsManifest::load(&dir.join(MODELS_MANIFEST)).unwrap();
        m.validate().unwrap();
        let ids: Vec<&str> = m.model.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "bge-m3-int8",
                "bge-m3-fp16",
                "minilm-l6-int8",
                "bge-reranker-v2-m3",
                "jina-reranker-v2"
            ]
        );
        let get = |id: &str| m.model.iter().find(|e| e.id == id).unwrap();
        let jina = get("jina-reranker-v2");
        assert!(jina.requires_license_ack && !jina.bundle);
        assert_eq!(jina.license, "CC-BY-NC-4.0");
        let test = m.select(&[], true, false).unwrap();
        assert_eq!(test.len(), 1);
        assert_eq!(test[0].id, "minilm-l6-int8");
        assert_eq!(test[0].spec.dim, Some(384));
        assert_eq!(get("bge-m3-int8").spec.dim, Some(1024));
        assert_eq!(get("bge-m3-fp16").device, ModelDevice::Gpu);
    }

    #[test]
    fn model_spec_toml_roundtrip() {
        let m = ModelsManifest::parse(&models_sample()).unwrap();
        let text = m.model[0].spec_toml().unwrap();
        let v: toml::Table = toml::from_str(&text).unwrap();
        assert_eq!(v["id"].as_str(), Some(m.model[0].id.as_str()));
        assert_eq!(v["dim"].as_integer(), Some(8));
        assert_eq!(v["pooling"].as_str(), Some("cls"));
        assert_eq!(v["sparse_head"].as_str(), Some("head.pt"));
        assert!(v.contains_key("max_tokens_passage") && v.contains_key("max_tokens_query"));
        assert_eq!(v["dynamic_quant"].as_bool(), Some(true));
        // у mean-модели без sparse-головы поля нет
        let t2: toml::Table = toml::from_str(&m.model[1].spec_toml().unwrap()).unwrap();
        assert_eq!(t2["pooling"].as_str(), Some("mean"));
        assert!(!t2.contains_key("sparse_head"));
        assert!(!t2.contains_key("dynamic_quant"));
    }

    // ---- deploy/cuda.toml ----

    /// Библиотеки, которые предзагружает `ort::ep::cuda::preload_dylibs`
    /// (ort 2.0.0-rc.14, `src/ep/cuda.rs`: `CUDA_DYLIBS` и `CUDNN_DYLIBS`).
    const ORT_DYLIBS_WINDOWS: &[&str] = &[
        "cudart64_13.dll",
        "cublasLt64_13.dll",
        "cublas64_13.dll",
        "cufft64_12.dll",
        "cudnn64_9.dll",
        "cudnn_graph64_9.dll",
        "cudnn_ops64_9.dll",
        "cudnn_heuristic64_9.dll",
        "cudnn_adv64_9.dll",
        "cudnn_cnn64_9.dll",
        "cudnn_engines_precompiled64_9.dll",
        "cudnn_engines_runtime_compiled64_9.dll",
    ];
    const ORT_DYLIBS_LINUX: &[&str] = &[
        "libcudart.so.13",
        "libcublasLt.so.13",
        "libcublas.so.13",
        "libnvrtc.so.13",
        "libcurand.so.10",
        "libcufft.so.12",
        "libcudnn.so.9",
        "libcudnn_graph.so.9",
        "libcudnn_ops.so.9",
        "libcudnn_heuristic.so.9",
        "libcudnn_adv.so.9",
        "libcudnn_cnn.so.9",
        "libcudnn_engines_precompiled.so.9",
        "libcudnn_engines_runtime_compiled.so.9",
    ];

    const CUDA_SAMPLE: &str = r#"
cuda = "13.4.3"
cudnn = "9.27.0"
[[lib]]
platform = "windows-x64"
component = "cuda_cudart"
version = "13.4.92"
url = "https://developer.download.nvidia.com/compute/cuda/redist/cuda_cudart/windows-x86_64/a.zip"
size = 10
sha256 = "1111111111111111111111111111111111111111111111111111111111111111"
extract = ["a/bin/x64/cudart64_13.dll"]
[[lib]]
platform = "linux-x64"
component = "cuda_cudart"
version = "13.4.92"
url = "https://developer.download.nvidia.com/compute/cuda/redist/cuda_cudart/linux-x86_64/a.tar.xz"
size = 10
sha256 = "2222222222222222222222222222222222222222222222222222222222222222"
extract = ["a/lib/libcudart.so.13"]
"#;

    #[test]
    fn cuda_manifest_parses_and_validates() {
        let m = CudaManifest::parse(CUDA_SAMPLE).unwrap();
        m.validate().unwrap();
        assert_eq!(m.cuda, "13.4.3");
        assert_eq!(m.cudnn, "9.27.0");
        assert_eq!(m.lib.len(), 2);
        assert_eq!(m.lib[0].component, "cuda_cudart");
        assert_eq!(m.lib[0].size, 10);
        assert_eq!(m.dest_names("linux-x64").unwrap(), ["libcudart.so.13"]);
        let ones = "1".repeat(64);
        for (from, to, needle) in [
            ("https://developer", "http://developer", "https://"),
            (
                "https://developer.download.nvidia.com",
                "https://example.com",
                "developer.download.nvidia.com",
            ),
            (ones.as_str(), "123", "sha256"),
            ("platform = \"linux-x64\"", "platform = \"mac\"", "mac"),
            (
                "extract = [\"a/lib/libcudart.so.13\"]",
                "extract = []",
                "extract",
            ),
            ("a/bin/x64/cudart64_13.dll", "a/bin/x64/", "a/bin/x64/"),
        ] {
            let bad = CUDA_SAMPLE.replacen(from, to, 1);
            let err = match CudaManifest::parse(&bad) {
                Ok(m) => format!("{:#}", m.validate().expect_err(from)),
                Err(e) => format!("{e:#}"),
            };
            assert!(err.contains(needle), "'{from}' -> '{to}': {err}");
        }
        // одно имя назначения из двух архивов одной платформы
        let win = CUDA_SAMPLE.split("[[lib]]").nth(1).unwrap();
        let dup = format!("{CUDA_SAMPLE}[[lib]]{win}");
        let err = format!(
            "{:#}",
            CudaManifest::parse(&dup).unwrap().validate().unwrap_err()
        );
        assert!(err.contains("cudart64_13.dll"), "{err}");
    }

    #[test]
    fn real_cuda_manifest_covers_ort_dylibs() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("deploy");
        let m = CudaManifest::load(&dir.join(CUDA_MANIFEST)).unwrap();
        m.validate().unwrap();
        for (plat, need) in [
            ("windows-x64", ORT_DYLIBS_WINDOWS),
            ("linux-x64", ORT_DYLIBS_LINUX),
        ] {
            let names = m.dest_names(plat).unwrap();
            for n in need {
                assert!(names.iter().any(|x| x == n), "{plat}: нет {n}");
            }
        }
    }
}
