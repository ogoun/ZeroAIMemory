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

/// Суффикс файла-кэша [`sha8`] рядом с моделью.
const SHA8_SUFFIX: &str = "sha8";

/// Ключ кэша: размер и время изменения файла (нс от эпохи).
fn sha8_key(path: &Path) -> Option<String> {
    let m = std::fs::metadata(path).ok()?;
    let t = m
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    Some(format!("{} {}", m.len(), t.as_nanos()))
}

/// Первые 8 hex-символов SHA-256 файла: часть `model_id`, меняется при замене весов.
///
/// Хэш файла модели (0,5–1,1 ГБ) при каждом старте — секунды на HDD (замер задачи 15 M2:
/// fp16 — до 11 с при холодном кэше), поэтому итог кэшируется рядом с файлом в
/// `<файл>.sha8` («<размер> <mtime, нс> <sha8>»). Замена файла (`models fetch`, копирование)
/// меняет размер или время изменения, и хэш считается заново. Каталог только для чтения —
/// без кэша. Целостность весов проверяет `models fetch` по sha256 манифеста; здесь — только
/// идентификатор.
pub(crate) fn sha8(path: &Path) -> Result<String, MlError> {
    let key = sha8_key(path);
    let mut side = path.as_os_str().to_owned();
    side.push(".");
    side.push(SHA8_SUFFIX);
    let side = PathBuf::from(side);
    if let (Some(k), Ok(text)) = (&key, std::fs::read_to_string(&side))
        && let Some((cached_key, v)) = text.trim().rsplit_once(' ')
        && cached_key == k
        && v.len() == 8
        && v.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Ok(v.to_string());
    }
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
    let v: String = d[..4].iter().map(|b| format!("{b:02x}")).collect();
    // ключ — до чтения: файл, изменённый во время хэширования, кэш не примет
    if let Some(k) = key
        && sha8_key(path).as_deref() == Some(k.as_str())
    {
        let _ = std::fs::write(&side, format!("{k} {v}\n"));
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join("unit-tmp")
            .join(format!("zam-ml-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn sha8_is_cached_by_size_and_mtime() {
        let d = dir("sha8");
        let f = d.join("model.onnx");
        std::fs::write(&f, b"weights v1").unwrap();
        let v1 = sha8(&f).unwrap();
        // sha256("weights v1") — первые 4 байта
        let full = Sha256::digest(b"weights v1");
        let want: String = full[..4].iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(v1, want);
        let side = d.join("model.onnx.sha8");
        let text = std::fs::read_to_string(&side).unwrap();
        assert!(text.trim().ends_with(&v1), "{text}");
        // кэш используется: подменённое значение при том же ключе возвращается как есть
        let (k, _) = text.trim().rsplit_once(' ').unwrap();
        std::fs::write(&side, format!("{k} deadbeef\n")).unwrap();
        assert_eq!(sha8(&f).unwrap(), "deadbeef");
        // замена файла (другой размер) — хэш заново
        std::fs::write(&f, b"weights version 2").unwrap();
        let v2 = sha8(&f).unwrap();
        assert_ne!(v2, "deadbeef");
        assert_ne!(v2, v1);
        // мусор в кэше — не ломает
        std::fs::write(&side, "мусор").unwrap();
        assert_eq!(sha8(&f).unwrap(), v2);
        let _ = std::fs::remove_dir_all(&d);
    }
}
