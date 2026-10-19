//! Манифест моделей `deploy/models.toml` (§8.4) и установка модели в `<models_dir>/<id>/`.
//!
//! Общий для `cargo xtask fetch-models` и `zam-server models fetch|status`: один разбор,
//! одна проверка и одна загрузка. Модель с `requires_license_ack` (реранкер jina,
//! CC-BY-NC-4.0) скачивается только с явным согласием — иначе [`FetchError::LicenseNotAccepted`]
//! до первой загрузки.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{FetchError, fetch_file};

/// Описатель модели для zam-ml в её каталоге.
pub const MODEL_SPEC_FILE: &str = "model.toml";
/// Лицензия весов (SPDX и ссылки) в каталоге модели.
pub const MODEL_LICENSE_FILE: &str = "LICENSE";

/// Назначение модели.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelRole {
    Embed,
    Rerank,
    Test,
}

/// Устройство, для которого предназначен вариант модели.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
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
    pub spec: ManifestSpec,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelFile {
    pub url: String,
    /// Имя файла в каталоге модели.
    pub path: String,
    /// Размер в байтах: сверяется при загрузке и в `models status`.
    pub size: u64,
    pub sha256: String,
}

/// `[model.spec]` манифеста — описатель модели для zam-ml.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestSpec {
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

/// Содержимое `<models_dir>/<id>/model.toml`, которое читает zam-ml.
#[derive(Serialize)]
struct ModelSpecFile<'a> {
    id: &'a str,
    /// embed | rerank | test: реранкер не загрузится как эмбеддер и наоборот.
    role: ModelRole,
    /// cpu | gpu | any: CPU-вариант zam-ml не запускает на GPU.
    device: ModelDevice,
    #[serde(flatten)]
    spec: &'a ManifestSpec,
}

/// Источник redist-архивов NVIDIA (`deploy/cuda.toml`).
pub(crate) const NVIDIA_REDIST: &str = "https://developer.download.nvidia.com/compute/";

/// Проверка, что `h` — 64 hex-символа (SHA-256).
pub(crate) fn check_sha256(h: &str) -> Result<(), String> {
    if h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("sha256 должен быть 64 hex-символа, получено '{h}'"));
    }
    Ok(())
}

/// Простое имя файла или каталога: без разделителей, `..` и управляющих символов.
pub(crate) fn check_plain_name(kind: &str, name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "{kind} '{name}' должен быть простым именем (латиница, цифры, '-', '_', '.')"
        ))
    }
}

/// Ссылка на Hugging Face обязана быть закреплена коммитом: `/resolve/<40 hex>/`.
fn check_hf_revision(url: &str) -> Result<(), String> {
    let Some(rest) = url.strip_prefix("https://huggingface.co/") else {
        return Ok(());
    };
    let rev = rest
        .split('/')
        .skip_while(|s| *s != "resolve")
        .nth(1)
        .unwrap_or("");
    if rev.len() != 40 || !rev.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!(
            "{url}: ревизия Hugging Face должна быть хэшем коммита (/resolve/<40 hex>/)"
        ));
    }
    Ok(())
}

impl ModelsManifest {
    pub fn parse(text: &str) -> Result<Self, FetchError> {
        toml::from_str(text)
            .map_err(|e| FetchError::Invalid(format!("не удалось разобрать манифест моделей: {e}")))
    }

    pub fn load(path: &Path) -> Result<Self, FetchError> {
        let text = std::fs::read_to_string(path).map_err(FetchError::io(format!(
            "не удалось прочитать {}",
            path.display()
        )))?;
        Self::parse(&text).map_err(|e| FetchError::Invalid(format!("{}: {e}", path.display())))
    }

    pub fn validate(&self) -> Result<(), FetchError> {
        if self.model.is_empty() {
            return Err(FetchError::Invalid(
                "в манифесте нет ни одной записи [[model]]".into(),
            ));
        }
        let mut ids: Vec<&str> = Vec::new();
        for m in &self.model {
            m.validate()
                .map_err(|e| FetchError::Invalid(format!("модель '{}': {e}", m.id)))?;
            if ids.contains(&m.id.as_str()) {
                return Err(FetchError::Invalid(format!(
                    "модель '{}' описана дважды",
                    m.id
                )));
            }
            ids.push(&m.id);
        }
        Ok(())
    }

    /// Модель по id; неизвестная — ошибка со списком известных.
    pub fn get(&self, id: &str) -> Result<&ModelEntry, FetchError> {
        self.model.iter().find(|m| m.id == id).ok_or_else(|| {
            let known: Vec<&str> = self.model.iter().map(|m| m.id.as_str()).collect();
            FetchError::Invalid(format!(
                "неизвестная модель '{id}', есть: {}",
                known.join(", ")
            ))
        })
    }

    /// Выбор моделей для загрузки: явные `ids` плюс тестовые при `test`;
    /// без того и другого — комплект `zam-models` (`bundle = true`).
    /// Модели с `requires_license_ack` — только при `accept_nc` (иначе ошибка до загрузки).
    pub fn select(
        &self,
        ids: &[String],
        test: bool,
        accept_nc: bool,
    ) -> Result<Vec<&ModelEntry>, FetchError> {
        let mut out: Vec<&ModelEntry> = Vec::new();
        for id in ids {
            let m = self.get(id)?;
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
        check_license_ack(&out, accept_nc)?;
        Ok(out)
    }
}

/// Согласие с лицензией для всех моделей набора — до загрузки чего-либо.
pub fn check_license_ack(models: &[&ModelEntry], accept_nc: bool) -> Result<(), FetchError> {
    match models.iter().find(|m| m.requires_license_ack) {
        Some(m) if !accept_nc => Err(FetchError::LicenseNotAccepted {
            id: m.id.clone(),
            license: m.license.clone(),
        }),
        _ => Ok(()),
    }
}

impl ModelEntry {
    fn validate(&self) -> Result<(), String> {
        check_plain_name("id", &self.id)?;
        if self.license.trim().is_empty() {
            return Err("не задана license".into());
        }
        if self.requires_license_ack && self.bundle {
            return Err("requires_license_ack = true допустимо только при bundle = false".into());
        }
        if self.files.is_empty() {
            return Err("пустой список files".into());
        }
        let mut paths: Vec<&str> = Vec::new();
        for f in &self.files {
            let ctx = |e: String| format!("файл '{}': {e}", f.path);
            check_plain_name("path", &f.path).map_err(ctx)?;
            if !f.url.starts_with("https://") {
                return Err(ctx("url должен начинаться с https://".into()));
            }
            check_hf_revision(&f.url).map_err(ctx)?;
            check_sha256(f.sha256.trim()).map_err(ctx)?;
            if f.size == 0 {
                return Err(ctx("size должен быть больше нуля".into()));
            }
            if paths.contains(&f.path.as_str()) {
                return Err(format!("два файла с одним path '{}'", f.path));
            }
            paths.push(&f.path);
        }
        for need in ["model.onnx", "tokenizer.json"] {
            if !paths.contains(&need) {
                return Err(format!("в files нет обязательного файла {need}"));
            }
        }
        let s = &self.spec;
        if let Some(h) = &s.sparse_head
            && !paths.contains(&h.as_str())
        {
            return Err(format!("sparse_head '{h}' не найден среди files"));
        }
        if s.max_tokens_passage == 0 || s.max_tokens_query == 0 {
            return Err("max_tokens_passage и max_tokens_query должны быть больше нуля".into());
        }
        match self.role {
            ModelRole::Embed | ModelRole::Test => {
                if s.dim.is_none_or(|d| d == 0) {
                    return Err("для эмбеддера нужен spec.dim > 0".into());
                }
                if s.pooling.is_none() {
                    return Err("для эмбеддера нужен spec.pooling (cls | mean)".into());
                }
            }
            ModelRole::Rerank => {
                if s.dim.is_some() || s.pooling.is_some() || s.sparse_head.is_some() {
                    return Err(
                        "у реранкера не бывает spec.dim, spec.pooling и spec.sparse_head".into(),
                    );
                }
            }
        }
        Ok(())
    }

    /// Суммарный размер файлов, байт.
    pub fn total_size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    /// `model.toml` для каталога модели: `id`, `role`, `device` и поля `[model.spec]`.
    pub fn spec_toml(&self) -> Result<String, FetchError> {
        let body = toml::to_string(&ModelSpecFile {
            id: &self.id,
            role: self.role,
            device: self.device,
            spec: &self.spec,
        })
        .map_err(|e| FetchError::Invalid(format!("описатель модели {}: {e}", self.id)))?;
        Ok(format!(
            "# Описатель модели для zam-ml; создан из deploy/models.toml \
             (zam-server models fetch / cargo xtask fetch-models)\n{body}"
        ))
    }

    /// Репозитории Hugging Face, из которых взяты файлы (`https://huggingface.co/<org>/<repo>`),
    /// без повторов; для прочих источников — URL файла.
    pub fn sources(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for f in &self.files {
            let src = match f.url.strip_prefix("https://huggingface.co/") {
                Some(rest) => {
                    let mut it = rest.split('/');
                    match (it.next(), it.next()) {
                        (Some(org), Some(repo)) => format!("https://huggingface.co/{org}/{repo}"),
                        _ => f.url.clone(),
                    }
                }
                None => f.url.clone(),
            };
            if !out.contains(&src) {
                out.push(src);
            }
        }
        out
    }

    /// Текст `LICENSE` в каталоге модели: SPDX-идентификатор, ссылка на текст лицензии и
    /// источники весов (полный текст — в карточке модели).
    pub fn license_text(&self) -> String {
        let mut s = format!(
            "Модель: {}\nЛицензия весов: {} — https://spdx.org/licenses/{}.html\n",
            self.id, self.license, self.license
        );
        if self.requires_license_ack {
            s.push_str(
                "Лицензия разрешает только некоммерческое использование; модель загружена \
                 с явным согласием (--accept-nc-license).\n",
            );
        }
        s.push_str("Источник (карточка модели с условиями использования):\n");
        for src in self.sources() {
            s.push_str(&format!("  {src}\n"));
        }
        s
    }
}

/// Итог установки модели.
#[derive(Debug)]
pub struct FetchedModel {
    pub id: String,
    pub dir: PathBuf,
    /// (имя файла, был ли уже на месте — без загрузки).
    pub files: Vec<(String, bool)>,
}

/// Атомарная запись маленького файла (через `<имя>.part` в том же каталоге).
fn write_atomic(path: &Path, text: &str) -> Result<(), FetchError> {
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".part");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, text).map_err(FetchError::io(format!(
        "не удалось записать {}",
        tmp.display()
    )))?;
    std::fs::rename(&tmp, path).map_err(FetchError::io(format!(
        "не удалось записать {}",
        path.display()
    )))
}

/// Ставит модель `id` из манифеста в `<models_dir>/<id>/` (см. [`fetch_model_with`]).
pub fn fetch_model(
    manifest: &ModelsManifest,
    id: &str,
    models_dir: &Path,
    accept_nc: bool,
) -> Result<FetchedModel, FetchError> {
    fetch_model_with(manifest, id, models_dir, accept_nc, &mut |_| {})
}

/// Ставит модель `id`: каждый файл скачивается с проверкой размера и SHA-256 (файл на
/// месте с верным хэшем не перекачивается), затем пишутся `LICENSE` и последним —
/// `model.toml`. Пока `model.toml` нет, модель не считается установленной: он удаляется
/// до загрузки, так что прерванное обновление не оставляет смесь старых и новых файлов
/// под видом рабочей модели. `on_file` вызывается перед каждым файлом (прогресс в CLI).
/// Без `accept_nc` модель с `requires_license_ack` не скачивается вовсе.
pub fn fetch_model_with(
    manifest: &ModelsManifest,
    id: &str,
    models_dir: &Path,
    accept_nc: bool,
    on_file: &mut dyn FnMut(&ModelFile),
) -> Result<FetchedModel, FetchError> {
    let m = manifest.get(id)?;
    check_license_ack(&[m], accept_nc)?;
    m.validate()
        .map_err(|e| FetchError::Invalid(format!("модель '{}': {e}", m.id)))?;
    install(m, models_dir, accept_nc, on_file)
}

/// Установка проверенной записи манифеста (см. [`fetch_model_with`]).
fn install(
    m: &ModelEntry,
    models_dir: &Path,
    accept_nc: bool,
    on_file: &mut dyn FnMut(&ModelFile),
) -> Result<FetchedModel, FetchError> {
    check_license_ack(&[m], accept_nc)?;
    let dir = models_dir.join(&m.id);
    std::fs::create_dir_all(&dir).map_err(FetchError::io(format!("создание {}", dir.display())))?;
    let spec = dir.join(MODEL_SPEC_FILE);
    if spec.exists() {
        std::fs::remove_file(&spec)
            .map_err(FetchError::io(format!("удаление {}", spec.display())))?;
    }
    let mut files = Vec::with_capacity(m.files.len());
    for f in &m.files {
        on_file(f);
        let cached = fetch_file(&f.url, &f.sha256, Some(f.size), &dir.join(&f.path))
            .map_err(|e| e.context(&format!("{}: {}", m.id, f.path)))?;
        files.push((f.path.clone(), cached));
    }
    write_atomic(&dir.join(MODEL_LICENSE_FILE), &m.license_text())?;
    write_atomic(&spec, &m.spec_toml()?)?;
    Ok(FetchedModel {
        id: m.id.clone(),
        dir,
        files,
    })
}

/// Состояние модели на диске (`models status`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelState {
    /// `model.toml` и все файлы на месте, размеры совпали (хэш — только при `verify`).
    Installed,
    /// Каталога или `model.toml` нет.
    Missing,
    /// Установка неполная или файлы не совпали с манифестом: что не так.
    Broken(String),
}

/// Проверяет установку модели: файлы и размеры; с `verify` — ещё и SHA-256 (долго для
/// гигабайтных весов).
pub fn model_state(m: &ModelEntry, models_dir: &Path, verify: bool) -> ModelState {
    let dir = models_dir.join(&m.id);
    let spec_ok = dir.join(MODEL_SPEC_FILE).is_file();
    let mut problems: Vec<String> = Vec::new();
    let mut any = false;
    for f in &m.files {
        let p = dir.join(&f.path);
        match std::fs::metadata(&p) {
            Ok(md) if md.is_file() => {
                any = true;
                if md.len() != f.size {
                    problems.push(format!("{}: размер {} вместо {}", f.path, md.len(), f.size));
                } else if verify {
                    match crate::sha256_file(&p) {
                        Ok(h) if h.eq_ignore_ascii_case(f.sha256.trim()) => {}
                        Ok(_) => problems.push(format!("{}: SHA-256 не совпал", f.path)),
                        Err(e) => problems.push(format!("{}: {e}", f.path)),
                    }
                }
            }
            _ => problems.push(format!("нет {}", f.path)),
        }
    }
    if !spec_ok && !any {
        return ModelState::Missing;
    }
    if !spec_ok {
        problems.push(format!("нет {MODEL_SPEC_FILE} (загрузка не завершена)"));
    }
    if problems.is_empty() {
        ModelState::Installed
    } else {
        ModelState::Broken(problems.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

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
            Ok(m) => m
                .validate()
                .expect_err(&format!("ожидалась ошибка: {from} -> {to}"))
                .to_string(),
            Err(e) => e.to_string(),
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
        assert_eq!(e.total_size(), 9);
        assert_eq!(e.spec.dim, Some(8));
        assert_eq!(e.spec.pooling, Some(Pooling::Cls));
        assert_eq!(e.spec.sparse_head.as_deref(), Some("head.pt"));
        assert_eq!(
            (e.spec.max_tokens_passage, e.spec.max_tokens_query),
            (16, 8)
        );
        assert_eq!(m.model[1].spec.pooling, Some(Pooling::Mean));
        assert!(m.model[2].requires_license_ack);
        assert_eq!(e.sources(), ["https://huggingface.co/o/r"]);
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
        let err = ModelsManifest::parse(text)
            .unwrap()
            .validate()
            .unwrap_err()
            .to_string();
        assert!(err.contains("files"), "{err}");
        let err = ModelsManifest::parse("")
            .unwrap()
            .validate()
            .unwrap_err()
            .to_string();
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

    fn deploy() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("deploy")
    }

    #[test]
    fn real_models_manifest_is_valid() {
        let m = ModelsManifest::load(&deploy().join("models.toml")).unwrap();
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
        let jina = m.get("jina-reranker-v2").unwrap();
        assert!(jina.requires_license_ack && !jina.bundle);
        assert_eq!(jina.license, "CC-BY-NC-4.0");
        assert_eq!(jina.device, ModelDevice::Cpu);
        let bge = m.get("bge-reranker-v2-m3").unwrap();
        assert!(bge.bundle && !bge.requires_license_ack);
        assert_eq!(
            (bge.role, bge.device),
            (ModelRole::Rerank, ModelDevice::Gpu)
        );
        let test = m.select(&[], true, false).unwrap();
        assert_eq!(test.len(), 1);
        assert_eq!(test[0].id, "minilm-l6-int8");
        assert_eq!(test[0].spec.dim, Some(384));
        assert_eq!(m.get("bge-m3-int8").unwrap().spec.dim, Some(1024));
        assert_eq!(m.get("bge-m3-fp16").unwrap().device, ModelDevice::Gpu);
        assert_eq!(
            jina.sources(),
            ["https://huggingface.co/jinaai/jina-reranker-v2-base-multilingual"]
        );
    }

    #[test]
    fn model_spec_toml_roundtrip() {
        let m = ModelsManifest::parse(&models_sample()).unwrap();
        let text = m.model[0].spec_toml().unwrap();
        let v: toml::Table = toml::from_str(&text).unwrap();
        assert_eq!(v["id"].as_str(), Some(m.model[0].id.as_str()));
        assert_eq!(v["role"].as_str(), Some("embed"));
        assert_eq!(v["dim"].as_integer(), Some(8));
        assert_eq!(v["device"].as_str(), Some("cpu"));
        assert_eq!(v["pooling"].as_str(), Some("cls"));
        assert_eq!(v["sparse_head"].as_str(), Some("head.pt"));
        assert!(v.contains_key("max_tokens_passage") && v.contains_key("max_tokens_query"));
        assert_eq!(v["dynamic_quant"].as_bool(), Some(true));
        // у mean-модели без sparse-головы поля нет
        let t2: toml::Table = toml::from_str(&m.model[1].spec_toml().unwrap()).unwrap();
        assert_eq!(t2["pooling"].as_str(), Some("mean"));
        assert!(!t2.contains_key("sparse_head"));
        assert!(!t2.contains_key("dynamic_quant"));
        let r: toml::Table = toml::from_str(&m.model[2].spec_toml().unwrap()).unwrap();
        assert_eq!(r["role"].as_str(), Some("rerank"));
        assert!(!r.contains_key("dim"));
    }

    // ---- загрузка модели с локального HTTP-сервера ----

    fn tmp(name: &str) -> PathBuf {
        let d = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join("test-tmp")
            .join("zam-fetch")
            .join(name);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Локальный HTTP-сервер: `/<имя>` → тело из `files`, остальное — 404. Считает запросы.
    fn serve(files: Vec<(&'static str, &'static [u8])>) -> (String, Arc<AtomicUsize>) {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let hits = Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        std::thread::spawn(move || {
            for s in l.incoming() {
                let Ok(mut s) = s else { return };
                let mut req = Vec::new();
                let mut b = [0u8; 1024];
                while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                    match std::io::Read::read(&mut s, &mut b) {
                        Ok(0) | Err(_) => break,
                        Ok(k) => req.extend_from_slice(&b[..k]),
                    }
                }
                let line = String::from_utf8_lossy(&req);
                let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
                h.fetch_add(1, Ordering::SeqCst);
                match files.iter().find(|(n, _)| path == format!("/{n}")) {
                    Some((_, body)) => {
                        let head = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = s.write_all(head.as_bytes());
                        let _ = s.write_all(body);
                    }
                    None => {
                        let _ = s.write_all(
                            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        );
                    }
                }
            }
        });
        (base, hits)
    }

    fn sha(b: &[u8]) -> String {
        use sha2::Digest as _;
        sha2::Sha256::digest(b)
            .iter()
            .map(|x| format!("{x:02x}"))
            .collect()
    }

    /// Манифест из одной модели `id` с файлами на локальном сервере `base`.
    /// `sha_override` подменяет хэш tokenizer.json.
    fn local_manifest(
        base: &str,
        id: &str,
        nc: bool,
        sha_override: Option<&str>,
    ) -> ModelsManifest {
        let (license, ack) = if nc {
            ("CC-BY-NC-4.0", "requires_license_ack = true\n")
        } else {
            ("Apache-2.0", "")
        };
        let tok_sha = sha_override.map_or_else(|| sha(b"TOKENIZER"), str::to_string);
        let text = format!(
            "[[model]]\nid = \"{id}\"\nrole = \"rerank\"\ndevice = \"any\"\nlicense = \"{license}\"\n\
             bundle = false\n{ack}files = [\n\
             {{ url = \"{base}/model.onnx\", path = \"model.onnx\", size = 5, sha256 = \"{}\" }},\n\
             {{ url = \"{base}/tokenizer.json\", path = \"tokenizer.json\", size = 9, sha256 = \"{tok_sha}\" }},\n\
             ]\n[model.spec]\nmax_tokens_passage = 64\nmax_tokens_query = 16\n",
            sha(b"MODEL")
        );
        // локальный http:// валидатор не пропустит: проверка только разбора
        ModelsManifest::parse(&text).unwrap()
    }

    #[test]
    fn fetch_model_writes_files_spec_and_license() {
        let d = tmp("fetch_model_ok");
        let (base, hits) = serve(vec![
            ("model.onnx", b"MODEL"),
            ("tokenizer.json", b"TOKENIZER"),
        ]);
        let m = local_manifest(&base, "r1", false, None);
        // http:// на петле validate() отвергает: в тесте — установка без проверки манифеста
        assert!(m.validate().is_err());
        let got = fetch_model_unchecked(&m, "r1", &d, false).unwrap();
        assert_eq!(
            got.files,
            [
                ("model.onnx".into(), false),
                ("tokenizer.json".into(), false)
            ]
        );
        let dir = d.join("r1");
        assert_eq!(std::fs::read(dir.join("model.onnx")).unwrap(), b"MODEL");
        assert_eq!(
            std::fs::read(dir.join("tokenizer.json")).unwrap(),
            b"TOKENIZER"
        );
        let spec: toml::Table =
            toml::from_str(&std::fs::read_to_string(dir.join(MODEL_SPEC_FILE)).unwrap()).unwrap();
        assert_eq!(spec["id"].as_str(), Some("r1"));
        assert_eq!(spec["role"].as_str(), Some("rerank"));
        let lic = std::fs::read_to_string(dir.join(MODEL_LICENSE_FILE)).unwrap();
        assert!(
            lic.contains("Apache-2.0") && lic.contains("spdx.org"),
            "{lic}"
        );
        assert_eq!(model_state(&m.model[0], &d, true), ModelState::Installed);
        // повторно — без сети
        let n = hits.load(Ordering::SeqCst);
        let again = fetch_model_unchecked(&m, "r1", &d, false).unwrap();
        assert!(again.files.iter().all(|(_, cached)| *cached));
        assert_eq!(hits.load(Ordering::SeqCst), n);
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["LICENSE", "model.onnx", "model.toml", "tokenizer.json"]
        );
    }

    #[test]
    fn fetch_model_bad_sha_leaves_no_partial_files() {
        let d = tmp("fetch_model_bad");
        let (base, _) = serve(vec![
            ("model.onnx", b"MODEL"),
            ("tokenizer.json", b"TOKENIZER"),
        ]);
        let wrong = "0".repeat(64);
        let m = local_manifest(&base, "r2", false, Some(&wrong));
        let err = fetch_model_unchecked(&m, "r2", &d, false).unwrap_err();
        // вариант ошибки сохраняется, контекст — модель и файл
        assert!(matches!(err, FetchError::Sha256Mismatch { .. }), "{err:?}");
        let err = err.to_string();
        assert!(
            err.contains(&wrong) && err.contains("tokenizer.json"),
            "{err}"
        );
        let dir = d.join("r2");
        // нет ни tokenizer.json, ни *.part, ни model.toml: модель не установлена
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["model.onnx"], "{names:?}");
        assert!(matches!(
            model_state(&m.model[0], &d, false),
            ModelState::Broken(_)
        ));
    }

    #[test]
    fn nc_model_without_ack_downloads_nothing() {
        let d = tmp("fetch_model_nc");
        let (base, hits) = serve(vec![
            ("model.onnx", b"MODEL"),
            ("tokenizer.json", b"TOKENIZER"),
        ]);
        let m = local_manifest(&base, "nc1", true, None);
        let err = fetch_model_unchecked(&m, "nc1", &d, false).unwrap_err();
        assert!(matches!(err, FetchError::LicenseNotAccepted { .. }));
        let s = err.to_string();
        assert!(
            s.contains("CC-BY-NC-4.0") && s.contains("--accept-nc-license"),
            "{s}"
        );
        // текст одной строкой, без серий пробелов от переноса в исходнике
        assert!(!s.contains("  ") && !s.contains('\n'), "{s}");
        assert_eq!(hits.load(Ordering::SeqCst), 0);
        assert!(!d.join("nc1").exists());
        assert_eq!(model_state(&m.model[0], &d, false), ModelState::Missing);
        // с согласием — ставится, LICENSE говорит о некоммерческом использовании
        fetch_model_unchecked(&m, "nc1", &d, true).unwrap();
        let lic = std::fs::read_to_string(d.join("nc1").join(MODEL_LICENSE_FILE)).unwrap();
        assert!(
            lic.contains("CC-BY-NC-4.0") && lic.contains("некоммерческое"),
            "{lic}"
        );
    }

    /// `fetch_model_with` без `validate()` (локальный http:// в тестовом манифесте).
    fn fetch_model_unchecked(
        m: &ModelsManifest,
        id: &str,
        dir: &Path,
        accept: bool,
    ) -> Result<FetchedModel, FetchError> {
        install(m.get(id)?, dir, accept, &mut |_| {})
    }
}
