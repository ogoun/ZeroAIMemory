//! Описатель модели `<models_dir>/<id>/model.toml` (пишет `cargo xtask fetch-models` / `zam-server
//! models fetch` из `[model.spec]` манифеста `deploy/models.toml`).

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::MlError;
use crate::models::{SPEC_FILE, model_dir};

/// Пулинг выхода `last_hidden_state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Pooling {
    /// Вектор первого токена (`<s>`/`[CLS]`) — bge-m3.
    Cls,
    /// Среднее по токенам под маской внимания — MiniLM.
    Mean,
}

/// Для какого устройства предназначен вариант модели (`device` в `deploy/models.toml`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelDevice {
    /// Только CPU (int8): с `ml.device = auto|cuda` всё равно работает на CPU.
    Cpu,
    Gpu,
    #[default]
    Any,
}

/// Описатель модели-эмбеддера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSpec {
    pub id: String,
    /// Размерность dense-вектора.
    pub dim: usize,
    pub pooling: Pooling,
    /// Абсолютный путь к sparse-голове (`sparse_linear.pt`), если она есть.
    pub sparse_head: Option<PathBuf>,
    /// Усечение чанка (со спецтокенами).
    pub max_tokens_passage: usize,
    /// Усечение запроса (со спецтокенами).
    pub max_tokens_query: usize,
    /// Активации квантуются динамически по всему тензору (int8 `DynamicQuantizeLinear`):
    /// соседи по батчу меняют вектор, поэтому тексты прогоняются по одному.
    pub dynamic_quant: bool,
    /// Устройство варианта модели; без поля в `model.toml` — `Any`.
    pub device: ModelDevice,
}

/// Назначение модели (`role` в `model.toml`; в описателях до M2-T9 поля нет).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Role {
    Embed,
    Rerank,
    Test,
    /// Эталон fp32 для eval (`cargo xtask eval --quality-gate`) — эмбеддер.
    Reference,
}

/// `model.toml` как он лежит на диске (поля `[model.spec]`, `id`, `role`, `device`).
#[derive(Deserialize)]
struct RawSpec {
    id: String,
    role: Option<Role>,
    dim: Option<usize>,
    pooling: Option<Pooling>,
    sparse_head: Option<String>,
    max_tokens_passage: usize,
    max_tokens_query: usize,
    #[serde(default)]
    dynamic_quant: bool,
    #[serde(default)]
    device: ModelDevice,
}

/// Имя файла без каталогов (защита от `../` в описателе).
fn is_plain_name(s: &str) -> bool {
    let p = Path::new(s);
    !s.is_empty() && p.file_name().is_some_and(|n| n == p.as_os_str())
}

/// Текст `<models_dir>/<id>/model.toml` и каталог модели. Нет каталога или файла — `NoModel`.
fn read_spec(models_dir: &Path, id: &str) -> Result<(String, PathBuf, PathBuf), MlError> {
    let dir = model_dir(models_dir, id)?;
    let path = dir.join(SPEC_FILE);
    match std::fs::read_to_string(&path) {
        Ok(t) => Ok((t, dir, path)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(MlError::NoModel(format!(
            "нет {} (zam-server models fetch)",
            path.display()
        ))),
        Err(e) => Err(MlError::Io(e)),
    }
}

/// Ошибка разбора с путём к описателю.
fn with_path(path: &Path) -> impl FnOnce(MlError) -> MlError + '_ {
    move |e| match e {
        MlError::Spec(m) => MlError::Spec(format!("{}: {m}", path.display())),
        e => e,
    }
}

impl ModelSpec {
    /// Читает `<models_dir>/<id>/model.toml`. Нет каталога или файла — `NoModel`, ошибка
    /// разбора или не эмбеддер (реранкер: `role = "rerank"` или нет `dim`/`pooling`) — `Spec`.
    pub fn load(models_dir: &Path, id: &str) -> Result<ModelSpec, MlError> {
        let (text, dir, path) = read_spec(models_dir, id)?;
        Self::parse(&text, &dir).map_err(with_path(&path))
    }

    /// Разбор текста `model.toml`; относительные пути отсчитываются от `dir`.
    pub fn parse(text: &str, dir: &Path) -> Result<ModelSpec, MlError> {
        let raw: RawSpec = toml::from_str(text).map_err(|e| MlError::Spec(e.to_string()))?;
        if raw.role == Some(Role::Rerank) {
            return Err(MlError::Spec(format!(
                "модель {} — реранкер, а не эмбеддер",
                raw.id
            )));
        }
        let (Some(dim), Some(pooling)) = (raw.dim, raw.pooling) else {
            return Err(MlError::Spec(format!(
                "модель {} не эмбеддер: нет dim или pooling",
                raw.id
            )));
        };
        if dim == 0 || raw.max_tokens_passage < 2 || raw.max_tokens_query < 2 {
            return Err(MlError::Spec(
                "dim > 0, max_tokens_passage и max_tokens_query ≥ 2".into(),
            ));
        }
        let sparse_head = match raw.sparse_head {
            Some(name) if is_plain_name(&name) => Some(dir.join(name)),
            Some(name) => {
                return Err(MlError::Spec(format!(
                    "sparse_head '{name}' — должно быть имя файла в каталоге модели"
                )));
            }
            None => None,
        };
        Ok(ModelSpec {
            id: raw.id,
            dim,
            pooling,
            sparse_head,
            max_tokens_passage: raw.max_tokens_passage,
            max_tokens_query: raw.max_tokens_query,
            dynamic_quant: raw.dynamic_quant,
            device: raw.device,
        })
    }
}

/// Описатель реранкера (cross-encoder): пара «запрос — фрагмент» → один логит.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RerankSpec {
    pub id: String,
    /// Предел пары целиком (со спецтокенами).
    pub max_tokens_passage: usize,
    /// Предел запроса (без спецтокенов пары).
    pub max_tokens_query: usize,
    /// См. [`ModelSpec::dynamic_quant`]: такие модели прогоняются по одной паре.
    pub dynamic_quant: bool,
    pub device: ModelDevice,
}

impl RerankSpec {
    /// Читает `<models_dir>/<id>/model.toml` реранкера. Нет каталога или файла — `NoModel`;
    /// эмбеддер (`role` не `rerank` или есть `dim`/`pooling`) — `Spec`.
    pub fn load(models_dir: &Path, id: &str) -> Result<RerankSpec, MlError> {
        let (text, _, path) = read_spec(models_dir, id)?;
        Self::parse(&text).map_err(with_path(&path))
    }

    pub fn parse(text: &str) -> Result<RerankSpec, MlError> {
        let raw: RawSpec = toml::from_str(text).map_err(|e| MlError::Spec(e.to_string()))?;
        let is_rerank = match raw.role {
            Some(r) => r == Role::Rerank,
            // описатель без role: реранкер — тот, у кого нет dim и pooling
            None => raw.dim.is_none() && raw.pooling.is_none(),
        };
        if !is_rerank || raw.dim.is_some() || raw.pooling.is_some() {
            return Err(MlError::Spec(format!(
                "модель {} не реранкер (role = \"rerank\", без dim и pooling)",
                raw.id
            )));
        }
        if raw.max_tokens_passage < 4 || raw.max_tokens_query < 1 {
            return Err(MlError::Spec(
                "max_tokens_passage ≥ 4 и max_tokens_query ≥ 1".into(),
            ));
        }
        Ok(RerankSpec {
            id: raw.id,
            max_tokens_passage: raw.max_tokens_passage,
            max_tokens_query: raw.max_tokens_query,
            dynamic_quant: raw.dynamic_quant,
            device: raw.device,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bge_spec() {
        let s = ModelSpec::parse(
            "id = \"bge\"\ndim = 1024\npooling = \"cls\"\nsparse_head = \"sparse_linear.pt\"\n\
             max_tokens_passage = 1024\nmax_tokens_query = 512\ndynamic_quant = true\ndevice = \"cpu\"\n",
            Path::new("m"),
        )
        .unwrap();
        assert_eq!(s.dim, 1024);
        assert_eq!(s.pooling, Pooling::Cls);
        assert!(s.dynamic_quant);
        assert_eq!(s.device, ModelDevice::Cpu);
        assert_eq!(
            s.sparse_head.as_deref(),
            Some(Path::new("m").join("sparse_linear.pt").as_path())
        );
        // fp32-эталон для eval (role = "reference") грузится как эмбеддер, но не как реранкер
        let reference = "id = \"bge-m3-fp32\"\nrole = \"reference\"\ndim = 1024\npooling = \"cls\"\n\
                         max_tokens_passage = 1024\nmax_tokens_query = 512\n";
        assert_eq!(
            ModelSpec::parse(reference, Path::new("m")).unwrap().device,
            ModelDevice::Any
        );
        assert!(RerankSpec::parse(reference).is_err());
    }

    #[test]
    fn rerank_spec_parses_and_rejects_embedders() {
        let r = RerankSpec::parse(
            "id = \"jina\"
role = \"rerank\"
device = \"cpu\"
max_tokens_passage = 1024
             max_tokens_query = 256
",
        )
        .unwrap();
        assert_eq!((r.max_tokens_passage, r.max_tokens_query), (1024, 256));
        assert_eq!(r.device, ModelDevice::Cpu);
        assert!(!r.dynamic_quant);
        // описатель без role (до M2-T9): по отсутствию dim и pooling
        assert!(
            RerankSpec::parse(
                "id = \"r\"
max_tokens_passage = 8
max_tokens_query = 8
"
            )
            .is_ok()
        );
        for bad in [
            "id = \"e\"
role = \"embed\"
max_tokens_passage = 8
max_tokens_query = 8
",
            "id = \"e\"
dim = 4
pooling = \"mean\"
max_tokens_passage = 8
max_tokens_query = 8
",
            "id = \"e\"
role = \"rerank\"
dim = 4
max_tokens_passage = 8
max_tokens_query = 8
",
            "id = \"e\"
role = \"rerank\"
max_tokens_passage = 2
max_tokens_query = 8
",
        ] {
            assert!(
                matches!(RerankSpec::parse(bad), Err(MlError::Spec(_))),
                "{bad}"
            );
        }
        // реранкер не загружается как эмбеддер даже с dim
        let e = ModelSpec::parse(
            "id = \"r\"
role = \"rerank\"
dim = 4
pooling = \"cls\"
max_tokens_passage = 8
max_tokens_query = 8
",
            Path::new("m"),
        );
        assert!(matches!(e, Err(MlError::Spec(m)) if m.contains("реранкер")));
    }

    #[test]
    fn rejects_reranker_and_paths() {
        for bad in [
            "id = \"r\"\nmax_tokens_passage = 8\nmax_tokens_query = 8\n",
            "id = \"r\"\ndim = 4\npooling = \"mean\"\nsparse_head = \"../x.pt\"\nmax_tokens_passage = 8\nmax_tokens_query = 8\n",
            "id = \"r\"\ndim = 0\npooling = \"mean\"\nmax_tokens_passage = 8\nmax_tokens_query = 8\n",
            "id = \"r\"\ndim = 4\npooling = \"max\"\nmax_tokens_passage = 8\nmax_tokens_query = 8\n",
        ] {
            assert!(
                matches!(ModelSpec::parse(bad, Path::new("m")), Err(MlError::Spec(_))),
                "{bad}"
            );
        }
    }
}
