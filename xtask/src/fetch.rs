//! Кэш архивов `target/deps-cache` и штампы распакованных зависимостей.
//! Загрузка, SHA-256 и извлечение — в общем крейте `zam-fetch` (их же использует сервер).

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use zam_fetch::{PlatformEntry, download, extract_atomic, verify_sha256};
pub use zam_fetch::{dest_names, sha256_file};

/// Скачивает во временный `*.part` внутри кэша; возвращает путь и хэш.
fn download_part(url: &str, cache: &Path) -> Result<(PathBuf, String)> {
    fs::create_dir_all(cache)?;
    let part = cache.join(format!("{}.part", std::process::id()));
    if let Err(e) = download(url, &part) {
        let _ = fs::remove_file(&part);
        return Err(e.into());
    }
    let hash = sha256_file(&part)?;
    Ok((part, hash))
}

/// Штамп распакованной зависимости: `<out>.sha256` рядом с каталогом, хранит хэш архива.
fn stamp_path(out: &Path) -> Result<PathBuf> {
    let mut n = out
        .file_name()
        .context("пустое имя каталога")?
        .to_os_string();
    n.push(".sha256");
    Ok(out.with_file_name(n))
}

/// Каталог `out` распакован из архива `expected` и все файлы на месте.
fn is_up_to_date(entry: &PlatformEntry, out: &Path, expected: &str) -> Result<bool> {
    let stamp = stamp_path(out)?;
    let Ok(text) = fs::read_to_string(&stamp) else {
        return Ok(false);
    };
    if text.trim() != expected {
        return Ok(false);
    }
    Ok(dest_names(entry)?.iter().all(|n| out.join(n).is_file()))
}

/// Скачивает (если нет в кэше), проверяет хэш и распаковывает. `root` — каталог `target/`.
/// Возвращает true, если загрузка не понадобилась (архив в кэше или каталог уже распакован
/// из того же архива — по штампу `<out>.sha256`; так CI достаточно кэшировать `target/deps`).
pub fn fetch_one(name: &str, entry: &PlatformEntry, root: &Path, out: &Path) -> Result<bool> {
    if entry.sha256.trim().is_empty() {
        bail!(
            "{name}: sha256 не задан в манифесте; получите значение через `cargo xtask fetch-deps --print-hashes`"
        );
    }
    let expected = entry.sha256.trim().to_ascii_lowercase();
    if is_up_to_date(entry, out, &expected)? {
        return Ok(true);
    }
    let stamp = stamp_path(out)?;
    if stamp.exists() {
        fs::remove_file(&stamp)?;
    }
    let cache = root.join("deps-cache");
    let cached = cache.join(&expected);
    let from_cache = cached.is_file();
    if from_cache {
        verify_sha256(&cached, &expected).with_context(|| {
            format!(
                "{name}: файл кэша повреждён; удалите файл кэша {} и повторите",
                cached.display()
            )
        })?;
    } else {
        println!("{name}: загрузка {}", entry.url);
        let (part, actual) = download_part(&entry.url, &cache)?;
        if !actual.eq_ignore_ascii_case(&expected) {
            let _ = fs::remove_file(&part);
            bail!("{name}: несовпадение SHA-256: ожидалось {expected}, получено {actual}");
        }
        fs::rename(&part, &cached)?;
    }
    extract_atomic(&cached, &entry.url, entry, out).with_context(|| name.to_string())?;
    fs::write(&stamp, format!("{expected}\n"))?;
    Ok(from_cache)
}

/// Скачивает архив, кладёт в кэш под фактическим хэшем и возвращает хэш.
pub fn print_hash(url: &str, root: &Path) -> Result<String> {
    let cache = root.join("deps-cache");
    let (part, hash) = download_part(url, &cache)?;
    fs::rename(&part, cache.join(&hash))?;
    Ok(hash)
}

#[cfg(test)]
mod tests {
    // Загрузка, хэши и извлечение проверяются в zam-fetch; здесь — кэш и штампы.
    use super::*;
    use std::collections::BTreeMap;
    use std::io::Write;

    fn tmp(name: &str) -> PathBuf {
        let d = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../target/test-tmp/xtask")
            .join(name);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn entry(extract: &[&str]) -> PlatformEntry {
        PlatformEntry {
            url: String::new(),
            sha256: String::new(),
            size: None,
            extract: extract.iter().map(|s| s.to_string()).collect(),
            rename: BTreeMap::new(),
        }
    }

    fn make_zip(path: &Path) {
        let mut z = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        let o = zip::write::SimpleFileOptions::default();
        for (n, c) in [("root/lib/a.dll", "AAA"), ("root/other.txt", "X")] {
            z.start_file(n, o).unwrap();
            z.write_all(c.as_bytes()).unwrap();
        }
        z.finish().unwrap();
    }

    #[test]
    fn empty_hash_is_error() {
        let d = tmp("empty_hash");
        let e = fetch_one("qdrant", &entry(&["x"]), &d, &d.join("out")).unwrap_err();
        assert!(e.to_string().contains("--print-hashes"), "{e}");
    }

    #[test]
    fn untrusted_cache_is_error_and_nothing_extracted() {
        let d = tmp("bad_cache");
        let hash = "a".repeat(64);
        std::fs::create_dir_all(d.join("deps-cache")).unwrap();
        std::fs::write(d.join("deps-cache").join(&hash), b"not the archive").unwrap();
        let mut e = entry(&["x"]);
        e.sha256 = hash;
        e.url = "https://x/a.zip".into();
        let out = d.join("out");
        let err = format!("{:#}", fetch_one("qdrant", &e, &d, &out).unwrap_err());
        assert!(err.contains("удалите файл кэша"), "{err}");
        assert!(!out.exists());
    }

    #[test]
    fn fetch_one_skips_when_stamp_matches() {
        let d = tmp("stamp");
        let zip_p = d.join("a.zip");
        make_zip(&zip_p);
        let hash = sha256_file(&zip_p).unwrap();
        std::fs::create_dir_all(d.join("deps-cache")).unwrap();
        std::fs::rename(&zip_p, d.join("deps-cache").join(&hash)).unwrap();
        let mut e = entry(&["root/lib/a.dll"]);
        e.sha256 = hash.clone();
        e.url = "https://x/a.zip".into();
        let out = d.join("deps").join("dep");
        // первая распаковка из кэша архивов пишет штамп рядом с каталогом
        assert!(fetch_one("dep", &e, &d, &out).unwrap());
        let stamp = d.join("deps").join("dep.sha256");
        assert_eq!(std::fs::read_to_string(&stamp).unwrap().trim(), hash);
        // без архива в кэше: штамп совпал и файлы на месте — ничего не скачиваем
        std::fs::remove_dir_all(d.join("deps-cache")).unwrap();
        assert!(fetch_one("dep", &e, &d, &out).unwrap());
        // файла нет — штамп не доверенный, нужна загрузка (url недоступен — ошибка)
        std::fs::remove_file(out.join("a.dll")).unwrap();
        e.url = "https://127.0.0.1:1/a.zip".into();
        assert!(fetch_one("dep", &e, &d, &out).is_err());
    }
}
