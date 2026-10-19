//! Раскладка каталога модели и её идентификатор.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};

use crate::MlError;

/// Описатель модели.
pub(crate) const SPEC_FILE: &str = "model.toml";
/// ONNX-граф.
pub(crate) const MODEL_FILE: &str = "model.onnx";
/// Токенизатор Hugging Face.
pub(crate) const TOKENIZER_FILE: &str = "tokenizer.json";

/// `<models_dir>/<id>`; `id` — одно имя каталога, без разделителей.
pub(crate) fn model_dir(models_dir: &Path, id: &str) -> Result<PathBuf, MlError> {
    let p = Path::new(id);
    if id.is_empty() || p.file_name().is_none_or(|n| n != p.as_os_str()) {
        return Err(MlError::Spec(format!("некорректный id модели '{id}'")));
    }
    let dir = models_dir.join(id);
    if !dir.is_dir() {
        return Err(MlError::NoModel(format!(
            "нет каталога {} (zam-server models fetch)",
            dir.display()
        )));
    }
    Ok(dir)
}

/// Файл модели должен существовать, иначе `NoModel`.
pub(crate) fn require_file(path: &Path) -> Result<(), MlError> {
    if path.is_file() {
        Ok(())
    } else {
        Err(MlError::NoModel(format!("нет файла {}", path.display())))
    }
}

/// Первые 8 hex-символов SHA-256 файла: часть `model_id`, меняется при замене весов.
pub(crate) fn sha8(path: &Path) -> Result<String, MlError> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    let d = h.finalize();
    Ok(d[..4].iter().map(|b| format!("{b:02x}")).collect())
}
