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
}

/// `model.toml` как он лежит на диске (поля `[model.spec]` и `id`).
#[derive(Deserialize)]
struct RawSpec {
    id: String,
    dim: Option<usize>,
    pooling: Option<Pooling>,
    sparse_head: Option<String>,
    max_tokens_passage: usize,
    max_tokens_query: usize,
    #[serde(default)]
    dynamic_quant: bool,
}

/// Имя файла без каталогов (защита от `../` в описателе).
fn is_plain_name(s: &str) -> bool {
    let p = Path::new(s);
    !s.is_empty() && p.file_name().is_some_and(|n| n == p.as_os_str())
}

impl ModelSpec {
    /// Читает `<models_dir>/<id>/model.toml`. Нет каталога или файла — `NoModel`, ошибка
    /// разбора или не эмбеддер (реранкер без `dim`/`pooling`) — `Spec`.
    pub fn load(models_dir: &Path, id: &str) -> Result<ModelSpec, MlError> {
        let dir = model_dir(models_dir, id)?;
        let path = dir.join(SPEC_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(MlError::NoModel(format!(
                    "нет {} (zam-server models fetch)",
                    path.display()
                )));
            }
            Err(e) => return Err(MlError::Io(e)),
        };
        Self::parse(&text, &dir).map_err(|e| match e {
            MlError::Spec(m) => MlError::Spec(format!("{}: {m}", path.display())),
            e => e,
        })
    }

    /// Разбор текста `model.toml`; относительные пути отсчитываются от `dir`.
    pub fn parse(text: &str, dir: &Path) -> Result<ModelSpec, MlError> {
        let raw: RawSpec = toml::from_str(text).map_err(|e| MlError::Spec(e.to_string()))?;
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
             max_tokens_passage = 1024\nmax_tokens_query = 512\ndynamic_quant = true\n",
            Path::new("m"),
        )
        .unwrap();
        assert_eq!(s.dim, 1024);
        assert_eq!(s.pooling, Pooling::Cls);
        assert!(s.dynamic_quant);
        assert_eq!(
            s.sparse_head.as_deref(),
            Some(Path::new("m").join("sparse_linear.pt").as_path())
        );
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
