//! Метка хранилища `zam-storage.json` и стирание `storage/` с флагом пересборки.
//!
//! Совместимость хранилища Qdrant определяется версией внутреннего крейта `collection`, а
//! не SemVer Qdrant (исследование 05 §6.1), и при несовместимости процесс падает с panic.
//! Поэтому до запуска сверяем свою метку — версию Qdrant, создавшую `storage/`. Индекс
//! производный (источник истины — SQLite), так что несовместимое хранилище стирается и
//! пересобирается из `embeddings` без инференса.
//!
//! Флаг пересборки — файл `.zam-reindex`: пишется **до** стирания и снимается только
//! после подтверждения потребителем (`Supervisor::ack_storage_rebuilt`), так что падение
//! между стиранием и постановкой reindex не теряет пересборку.

use std::io;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Метка хранилища.
pub(crate) const STAMP_FILE: &str = "zam-storage.json";
/// Флаг «хранилище стёрто, нужна пересборка из embeddings».
pub(crate) const REINDEX_MARKER: &str = ".zam-reindex";
/// Каталог хранилища внутри `<data_dir>/qdrant/`.
pub(crate) const STORAGE_DIR: &str = "storage";
/// Временные файлы Qdrant (`storage.temp_path`).
const TEMP_DIR: &str = "temp";

#[derive(Debug, Serialize, Deserialize)]
struct Stamp {
    qdrant_version: String,
}

/// Версия из метки; `Some("")` — метка повреждена (считается несовместимой).
pub(crate) fn read_stamp(qdir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(qdir.join(STAMP_FILE)).ok()?;
    Some(
        serde_json::from_str::<Stamp>(&text)
            .map(|s| s.qdrant_version)
            .unwrap_or_default(),
    )
}

pub(crate) fn write_stamp(qdir: &Path, version: &str) -> io::Result<()> {
    let body = serde_json::to_string(&Stamp {
        qdrant_version: version.to_string(),
    })
    .map_err(io::Error::other)?;
    let tmp = qdir.join(format!("{STAMP_FILE}.tmp"));
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, qdir.join(STAMP_FILE))
}

fn parse(v: &str) -> Option<(u64, u64, u64)> {
    let mut it = v.trim().split('.').map(|p| p.parse::<u64>().ok());
    let r = (it.next()??, it.next()??, it.next()??);
    it.next().is_none().then_some(r)
}

/// Хранилище версии `stored` открывается версией `current`: та же major, рост не больше
/// чем на одну минорную, без отката. Нечитаемая версия — несовместима.
pub(crate) fn compatible(stored: &str, current: &str) -> bool {
    let (Some(s), Some(c)) = (parse(stored), parse(current)) else {
        return false;
    };
    s <= c && s.0 == c.0 && c.1 - s.1 <= 1
}

pub(crate) fn reindex_pending(qdir: &Path) -> bool {
    qdir.join(REINDEX_MARKER).exists()
}

pub(crate) fn mark_reindex(qdir: &Path, reason: &str) -> io::Result<()> {
    std::fs::write(qdir.join(REINDEX_MARKER), format!("{reason}\n"))
}

pub(crate) fn clear_reindex(qdir: &Path) -> io::Result<()> {
    match std::fs::remove_file(qdir.join(REINDEX_MARKER)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// `remove_dir_all` с повторами: на Windows дескрипторы только что завершённого процесса
/// освобождаются не мгновенно.
fn remove_dir_retry(p: &Path) -> io::Result<()> {
    let mut last = None;
    for _ in 0..20 {
        match std::fs::remove_dir_all(p) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => last = Some(e),
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err(last.unwrap_or_else(|| io::Error::other("remove_dir_all")))
}

/// Стирает `storage/`, `temp/` и метку; флаг пересборки ставится первым.
/// Процесс Qdrant к этому моменту должен быть остановлен.
pub(crate) fn wipe_storage(qdir: &Path, reason: &str) -> io::Result<()> {
    mark_reindex(qdir, reason)?;
    remove_dir_retry(&qdir.join(STORAGE_DIR))?;
    remove_dir_retry(&qdir.join(TEMP_DIR))?;
    match std::fs::remove_file(qdir.join(STAMP_FILE)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    tracing::warn!(
        component = "qdrant",
        reason,
        "хранилище Qdrant стёрто: индекс будет пересобран из embeddings"
    );
    Ok(())
}

/// Сверка метки до запуска. Возвращает true, если нужна пересборка индекса
/// (хранилище стёрто сейчас или раньше и пересборка ещё не подтверждена).
pub(crate) fn check_before_start(qdir: &Path, current: &str) -> io::Result<bool> {
    let storage = qdir.join(STORAGE_DIR);
    match read_stamp(qdir) {
        Some(stored) if !compatible(&stored, current) => {
            let reason = if stored.is_empty() {
                "метка хранилища повреждена".to_string()
            } else {
                format!("хранилище Qdrant {stored} несовместимо с {current}")
            };
            wipe_storage(qdir, &reason)?;
        }
        // Метка есть, а хранилища нет — его удалили вручную; embeddings в SQLite остались.
        Some(_) if !storage.exists() => {
            mark_reindex(qdir, "каталог storage удалён")?;
            let _ = std::fs::remove_file(qdir.join(STAMP_FILE));
        }
        // Хранилище без метки (до M2) или свежая установка: решает детектор panic.
        _ => {}
    }
    Ok(reindex_pending(qdir))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> std::path::PathBuf {
        let d = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join("test-tmp")
            .join("zam-index-stamp")
            .join(name);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn compatibility_rules() {
        assert!(compatible("1.19.2", "1.19.2"));
        assert!(compatible("1.19.0", "1.19.2"));
        assert!(compatible("1.18.5", "1.19.2"));
        assert!(!compatible("1.17.9", "1.19.2"), "скачок через минорную");
        assert!(!compatible("1.16.0", "1.19.2"));
        assert!(!compatible("1.19.3", "1.19.2"), "откат патча");
        assert!(!compatible("1.20.0", "1.19.2"), "откат минорной");
        assert!(!compatible("0.19.2", "1.19.2"), "другая major");
        assert!(!compatible("", "1.19.2"));
        assert!(!compatible("1.19", "1.19.2"));
        assert!(!compatible("1.19.2.1", "1.19.2"));
    }

    #[test]
    fn wipe_on_incompatible_stamp_and_marker_lifecycle() {
        let q = dir("wipe");
        std::fs::create_dir_all(q.join("storage").join("collections")).unwrap();
        write_stamp(&q, "1.16.0").unwrap();
        assert_eq!(read_stamp(&q).as_deref(), Some("1.16.0"));
        assert!(check_before_start(&q, "1.19.2").unwrap());
        assert!(!q.join("storage").exists());
        assert!(read_stamp(&q).is_none());
        // Флаг держится до подтверждения.
        assert!(check_before_start(&q, "1.19.2").unwrap());
        clear_reindex(&q).unwrap();
        assert!(!check_before_start(&q, "1.19.2").unwrap());
    }

    #[test]
    fn compatible_stamp_keeps_storage_and_missing_storage_marks() {
        let q = dir("keep");
        std::fs::create_dir_all(q.join("storage")).unwrap();
        std::fs::write(q.join("storage").join("x"), b"1").unwrap();
        write_stamp(&q, "1.19.2").unwrap();
        assert!(!check_before_start(&q, "1.19.2").unwrap());
        assert!(q.join("storage").join("x").exists());
        std::fs::remove_dir_all(q.join("storage")).unwrap();
        assert!(check_before_start(&q, "1.19.2").unwrap());
    }

    #[test]
    fn corrupt_stamp_wipes() {
        let q = dir("corrupt");
        std::fs::create_dir_all(q.join("storage")).unwrap();
        std::fs::write(q.join(STAMP_FILE), b"{not json").unwrap();
        assert_eq!(read_stamp(&q).as_deref(), Some(""));
        assert!(check_before_start(&q, "1.19.2").unwrap());
        assert!(!q.join("storage").exists());
    }
}
