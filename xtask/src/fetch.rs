//! Загрузка, проверка SHA-256 и распаковка зависимостей.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::manifest::PlatformEntry;

/// SHA-256 файла в нижнем регистре (hex).
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f =
        File::open(path).with_context(|| format!("не удалось открыть {}", path.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

pub fn verify_sha256(path: &Path, expected: &str) -> Result<()> {
    let actual = sha256_file(path)?;
    if actual.eq_ignore_ascii_case(expected.trim()) {
        Ok(())
    } else {
        bail!(
            "несовпадение SHA-256 для {}: ожидалось {}, получено {}",
            path.display(),
            expected,
            actual
        )
    }
}

/// Скачивает `url` в `dest` (через прокси из окружения), с таймаутами.
fn download(url: &str, dest: &Path) -> Result<()> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        .timeout_global(Some(Duration::from_secs(900)))
        .build()
        .into();
    let resp = agent
        .get(url)
        .call()
        .with_context(|| format!("загрузка {url}"))?;
    let mut reader = resp.into_body().into_reader();
    let mut out =
        File::create(dest).with_context(|| format!("не удалось создать {}", dest.display()))?;
    io::copy(&mut reader, &mut out).with_context(|| format!("чтение {url}"))?;
    out.flush()?;
    Ok(())
}

/// Скачивает во временный `*.part` внутри кэша; возвращает путь и хэш.
fn download_part(url: &str, cache: &Path) -> Result<(PathBuf, String)> {
    fs::create_dir_all(cache)?;
    let part = cache.join(format!("{}.part", std::process::id()));
    if let Err(e) = download(url, &part) {
        let _ = fs::remove_file(&part);
        return Err(e);
    }
    let hash = sha256_file(&part)?;
    Ok((part, hash))
}

/// Путь внутри архива без `./` и с прямыми слэшами.
fn norm(p: &str) -> String {
    p.replace('\\', "/").trim_start_matches("./").to_string()
}

/// Имя файла назначения: rename или последний компонент пути; только простое имя.
fn dest_name(entry: &PlatformEntry, path: &str) -> Result<String> {
    let name = match entry.rename.get(path) {
        Some(n) => n.clone(),
        None => path.rsplit('/').next().unwrap_or(path).to_string(),
    };
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', ':']) {
        bail!("недопустимое имя назначения '{name}' для '{path}'");
    }
    Ok(name)
}

/// Извлекает из архива только перечисленные в `entry.extract` файлы в `out`.
pub fn extract_selected(
    archive: &Path,
    url: &str,
    entry: &PlatformEntry,
    out: &Path,
) -> Result<()> {
    let wanted: Vec<String> = entry.extract.iter().map(|p| norm(p)).collect();
    // Имена назначения проверяем до записи чего-либо.
    let names: Vec<String> = wanted
        .iter()
        .map(|p| dest_name(entry, p))
        .collect::<Result<_>>()?;
    fs::create_dir_all(out)?;
    let mut found = vec![false; wanted.len()];
    let lower = url.to_ascii_lowercase();
    if lower.ends_with(".zip") {
        let mut z = zip::ZipArchive::new(File::open(archive)?).context("не zip-архив")?;
        for (i, w) in wanted.iter().enumerate() {
            let mut f = match z.by_name(w) {
                Ok(f) => f,
                Err(_) => continue,
            };
            let mut o = File::create(out.join(&names[i]))?;
            io::copy(&mut f, &mut o)?;
            found[i] = true;
        }
    } else if lower.ends_with(".tgz") || lower.ends_with(".tar.gz") {
        let gz = flate2::read::GzDecoder::new(File::open(archive)?);
        let mut t = tar::Archive::new(gz);
        for e in t.entries()? {
            let mut e = e?;
            let p = norm(&e.path()?.to_string_lossy());
            if let Some(i) = wanted.iter().position(|w| *w == p) {
                let mut o = File::create(out.join(&names[i]))?;
                io::copy(&mut e, &mut o)?;
                found[i] = true;
            }
        }
    } else {
        bail!("неизвестный формат архива: {url}");
    }
    let missing: Vec<&str> = wanted
        .iter()
        .zip(&found)
        .filter(|(_, f)| !**f)
        .map(|(w, _)| w.as_str())
        .collect();
    if !missing.is_empty() {
        bail!("в архиве не найдены файлы: {}", missing.join(", "));
    }
    Ok(())
}

/// Скачивает (если нет в кэше), проверяет хэш и распаковывает. `root` — каталог `target/`.
/// Возвращает true, если файл взят из кэша.
pub fn fetch_one(name: &str, entry: &PlatformEntry, root: &Path, out: &Path) -> Result<bool> {
    if entry.sha256.trim().is_empty() {
        bail!(
            "{name}: sha256 не задан в манифесте; получите значение через `cargo xtask fetch-deps --print-hashes`"
        );
    }
    let expected = entry.sha256.trim().to_ascii_lowercase();
    let cache = root.join("deps-cache");
    let cached = cache.join(&expected);
    let from_cache = cached.is_file();
    if from_cache {
        // Кэш мог быть повреждён — проверяем и при ошибке не качаем молча.
        verify_sha256(&cached, &expected)?;
    } else {
        println!("{name}: загрузка {}", entry.url);
        let (part, actual) = download_part(&entry.url, &cache)?;
        if !actual.eq_ignore_ascii_case(&expected) {
            let _ = fs::remove_file(&part);
            bail!("{name}: несовпадение SHA-256: ожидалось {expected}, получено {actual}");
        }
        fs::rename(&part, &cached)?;
    }
    extract_selected(&cached, &entry.url, entry, out).with_context(|| name.to_string())?;
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

    fn entry(extract: &[&str], rename: &[(&str, &str)]) -> PlatformEntry {
        PlatformEntry {
            url: String::new(),
            sha256: String::new(),
            extract: extract.iter().map(|s| s.to_string()).collect(),
            rename: rename
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    fn make_zip(path: &Path) {
        let mut z = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        let o = zip::write::SimpleFileOptions::default();
        for (n, c) in [
            ("root/lib/a.dll", "AAA"),
            ("root/lib/b.dll", "BBB"),
            ("root/other.txt", "X"),
        ] {
            z.start_file(n, o).unwrap();
            z.write_all(c.as_bytes()).unwrap();
        }
        z.finish().unwrap();
    }

    fn make_tgz(path: &Path) {
        let gz = flate2::write::GzEncoder::new(
            std::fs::File::create(path).unwrap(),
            flate2::Compression::fast(),
        );
        let mut t = tar::Builder::new(gz);
        for (n, c) in [
            ("./root/lib/a.so", "AAA"),
            ("root/lib/b.so", "BBB"),
            ("root/other.txt", "X"),
        ] {
            let mut h = tar::Header::new_gnu();
            h.set_size(c.len() as u64);
            h.set_mode(0o644);
            t.append_data(&mut h, n, c.as_bytes()).unwrap();
        }
        t.into_inner().unwrap().finish().unwrap();
    }

    #[test]
    fn sha_mismatch_is_error() {
        let d = tmp("sha_mismatch");
        let f = d.join("f.bin");
        std::fs::write(&f, b"hello").unwrap();
        let actual = sha256_file(&f).unwrap();
        assert_eq!(
            actual,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        verify_sha256(&f, &actual).unwrap();
        let wrong = "0".repeat(64);
        let err = verify_sha256(&f, &wrong).unwrap_err().to_string();
        assert!(err.contains(&wrong) && err.contains(&actual), "{err}");
    }

    #[test]
    fn extract_selected_files_only() {
        let d = tmp("extract_selected");
        let zip_p = d.join("a.zip");
        make_zip(&zip_p);
        let out = d.join("out_zip");
        extract_selected(
            &zip_p,
            "https://x/a.zip",
            &entry(&["root/lib/a.dll"], &[]),
            &out,
        )
        .unwrap();
        assert_eq!(std::fs::read(out.join("a.dll")).unwrap(), b"AAA");
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 1);

        let tgz_p = d.join("a.tgz");
        make_tgz(&tgz_p);
        let out = d.join("out_tgz");
        extract_selected(
            &tgz_p,
            "https://x/a.tgz",
            &entry(
                &["root/lib/a.so", "root/lib/b.so"],
                &[("root/lib/b.so", "libb.so")],
            ),
            &out,
        )
        .unwrap();
        assert_eq!(std::fs::read(out.join("a.so")).unwrap(), b"AAA");
        assert_eq!(std::fs::read(out.join("libb.so")).unwrap(), b"BBB");
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 2);
    }

    #[test]
    fn missing_entry_and_bad_rename_are_errors() {
        let d = tmp("extract_errors");
        let zip_p = d.join("a.zip");
        make_zip(&zip_p);
        let e = extract_selected(&zip_p, "u.zip", &entry(&["root/nope"], &[]), &d.join("o1"))
            .unwrap_err();
        assert!(e.to_string().contains("root/nope"), "{e}");
        let e = extract_selected(
            &zip_p,
            "u.zip",
            &entry(&["root/lib/a.dll"], &[("root/lib/a.dll", "../evil")]),
            &d.join("o2"),
        )
        .unwrap_err();
        assert!(e.to_string().contains("../evil"), "{e}");
        assert!(!d.join("evil").exists());
    }

    #[test]
    fn empty_hash_is_error() {
        let d = tmp("empty_hash");
        let e = fetch_one("qdrant", &entry(&["x"], &[]), &d, &d.join("out")).unwrap_err();
        assert!(e.to_string().contains("--print-hashes"), "{e}");
    }
}
