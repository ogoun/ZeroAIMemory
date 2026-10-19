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

/// Адрес на локальной петле (`127.0.0.1`, `localhost`, `[::1]`): к нему прокси не применяется.
fn is_loopback(url: &str) -> bool {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    let host = if host.starts_with('[') {
        host.split_once(']').map_or(host, |(h, _)| &h[1..])
    } else {
        host.split(':').next().unwrap_or(host)
    };
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Скачивает `url` в `dest` (через прокси из окружения, кроме локальной петли), с таймаутами.
fn download(url: &str, dest: &Path) -> Result<()> {
    let mut cfg = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        // крупные модели (до 1,2 ГБ) на медленном канале
        .timeout_global(Some(Duration::from_secs(3600)));
    if is_loopback(url) {
        cfg = cfg.proxy(None);
    }
    let agent: ureq::Agent = cfg.build().into();
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

/// Имена назначения для всех `extract`; дубликаты и недопустимые имена — ошибка.
pub fn dest_names(entry: &PlatformEntry) -> Result<Vec<String>> {
    let mut names: Vec<String> = Vec::new();
    for p in &entry.extract {
        let n = dest_name(entry, &norm(p))?;
        if names.contains(&n) {
            bail!("два файла извлекаются под одним именем '{n}'");
        }
        names.push(n);
    }
    Ok(names)
}

/// Извлекает из архива только перечисленные в `entry.extract` обычные файлы в `out`.
pub fn extract_selected(
    archive: &Path,
    url: &str,
    entry: &PlatformEntry,
    out: &Path,
) -> Result<()> {
    let wanted: Vec<String> = entry.extract.iter().map(|p| norm(p)).collect();
    // Имена назначения проверяем до записи чего-либо.
    let names = dest_names(entry)?;
    fs::create_dir_all(out)?;
    let mut found = vec![false; wanted.len()];
    let lower = url.to_ascii_lowercase();
    if lower.ends_with(".zip") {
        let mut z = zip::ZipArchive::new(File::open(archive)?).context("не zip-архив")?;
        for (i, w) in wanted.iter().enumerate() {
            let mut f = match z.by_name(w) {
                Ok(f) => f,
                Err(zip::result::ZipError::FileNotFound) => continue,
                Err(e) => bail!("ошибка чтения '{w}' из zip: {e}"),
            };
            if !f.is_file() || f.is_symlink() {
                bail!("'{w}' в архиве не является обычным файлом");
            }
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
                if !e.header().entry_type().is_file() {
                    bail!("'{p}' в архиве не является обычным файлом");
                }
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

/// Атомарная замена каталога: распаковка в `<out>.tmp`, затем подмена `out`.
pub fn extract_atomic(archive: &Path, url: &str, entry: &PlatformEntry, out: &Path) -> Result<()> {
    let mut tmp_name = out
        .file_name()
        .context("пустое имя каталога")?
        .to_os_string();
    tmp_name.push(".tmp");
    let tmp = out.with_file_name(tmp_name);
    if tmp.exists() {
        fs::remove_dir_all(&tmp)?;
    }
    if let Err(e) = extract_selected(archive, url, entry, &tmp) {
        let _ = fs::remove_dir_all(&tmp);
        return Err(e);
    }
    if out.exists() {
        fs::remove_dir_all(out)?;
    }
    fs::rename(&tmp, out)?;
    Ok(())
}

/// Скачивает одиночный файл (без распаковки) в `dest` с проверкой SHA-256.
/// Загрузка идёт во временный `<имя>.<pid>.part` рядом с `dest`; при ошибке или
/// несовпадении хэша он удаляется. Если `dest` уже есть и хэш совпал — сеть не нужна,
/// возвращается true; файл с чужим хэшем удаляется и скачивается заново.
pub fn fetch_file(url: &str, sha256: &str, dest: &Path) -> Result<bool> {
    let expected = sha256.trim().to_ascii_lowercase();
    if dest.is_file() {
        if sha256_file(dest)? == expected {
            return Ok(true);
        }
        fs::remove_file(dest).with_context(|| format!("не удалось удалить {}", dest.display()))?;
    }
    let dir = dest.parent().context("у пути назначения нет каталога")?;
    fs::create_dir_all(dir)?;
    let mut part_name = dest.file_name().context("пустое имя файла")?.to_os_string();
    part_name.push(format!(".{}.part", std::process::id()));
    let part = dir.join(part_name);
    println!("  загрузка {url}");
    let checked = download(url, &part).and_then(|()| {
        let actual = sha256_file(&part)?;
        if actual == expected {
            Ok(())
        } else {
            bail!("несовпадение SHA-256 для {url}: ожидалось {expected}, получено {actual}")
        }
    });
    if let Err(e) = checked {
        let _ = fs::remove_file(&part);
        return Err(e);
    }
    fs::rename(&part, dest)
        .with_context(|| format!("не удалось переименовать в {}", dest.display()))?;
    Ok(false)
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
    use super::*;
    use std::collections::BTreeMap;
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};

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

    #[test]
    fn atomic_extract_replaces_dir_and_keeps_old_on_error() {
        let d = tmp("atomic");
        let zip_p = d.join("a.zip");
        make_zip(&zip_p);
        let out = d.join("dep");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("stale.txt"), b"old").unwrap();
        extract_atomic(&zip_p, "u.zip", &entry(&["root/lib/a.dll"], &[]), &out).unwrap();
        assert!(!out.join("stale.txt").exists());
        assert!(out.join("a.dll").exists());
        assert!(!d.join("dep.tmp").exists());

        let e = extract_atomic(
            &zip_p,
            "u.zip",
            &entry(&["root/lib/a.dll", "root/nope"], &[]),
            &out,
        );
        assert!(e.is_err());
        assert_eq!(std::fs::read(out.join("a.dll")).unwrap(), b"AAA");
        assert!(!d.join("dep.tmp").exists());
    }

    #[test]
    fn untrusted_cache_is_error_and_nothing_extracted() {
        let d = tmp("bad_cache");
        let hash = "a".repeat(64);
        std::fs::create_dir_all(d.join("deps-cache")).unwrap();
        std::fs::write(d.join("deps-cache").join(&hash), b"not the archive").unwrap();
        let mut e = entry(&["x"], &[]);
        e.sha256 = hash;
        e.url = "https://x/a.zip".into();
        let out = d.join("out");
        let err = format!("{:#}", fetch_one("qdrant", &e, &d, &out).unwrap_err());
        assert!(err.contains("удалите файл кэша"), "{err}");
        assert!(!out.exists());
    }

    /// Локальный HTTP-сервер: отдаёт `body` на каждый из `n` запросов, затем закрывается.
    /// Возвращает URL и счётчик обслуженных запросов.
    fn serve(body: &'static [u8], n: usize) -> (String, std::sync::Arc<AtomicUsize>) {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/file.bin", l.local_addr().unwrap());
        let hits = std::sync::Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        std::thread::spawn(move || {
            for _ in 0..n {
                let Ok((mut s, _)) = l.accept() else { return };
                let mut req = Vec::new();
                let mut b = [0u8; 1024];
                while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                    match io::Read::read(&mut s, &mut b) {
                        Ok(0) | Err(_) => break,
                        Ok(k) => req.extend_from_slice(&b[..k]),
                    }
                }
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(head.as_bytes());
                let _ = s.write_all(body);
                h.fetch_add(1, Ordering::SeqCst);
            }
        });
        (url, hits)
    }

    const HELLO_SHA: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

    #[test]
    fn fetch_file_downloads_verifies_and_reuses() {
        let d = tmp("fetch_file_ok");
        let (url, hits) = serve(b"hello", 1);
        let dest = d.join("m").join("model.onnx");
        assert!(!fetch_file(&url, HELLO_SHA, &dest).unwrap());
        assert_eq!(std::fs::read(&dest).unwrap(), b"hello");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        // повторный вызов: файл на месте и хэш совпал — без сети
        assert!(fetch_file(&url, HELLO_SHA, &dest).unwrap());
        // испорченный файл перекачивается (сервер уже закрыт — ошибка, файл удалён)
        std::fs::write(&dest, b"broken").unwrap();
        assert!(fetch_file(&url, HELLO_SHA, &dest).is_err());
        assert!(!dest.exists());
        assert_eq!(std::fs::read_dir(d.join("m")).unwrap().count(), 0);
    }

    #[test]
    fn fetch_file_sha_mismatch_removes_partial() {
        let d = tmp("fetch_file_bad_sha");
        let (url, _) = serve(b"hello", 1);
        let dest = d.join("m").join("model.onnx");
        let wrong = "0".repeat(64);
        let err = fetch_file(&url, &wrong, &dest).unwrap_err().to_string();
        assert!(err.contains(&wrong) && err.contains(HELLO_SHA), "{err}");
        assert!(!dest.exists());
        // временный *.part удалён, каталог пуст
        assert_eq!(std::fs::read_dir(d.join("m")).unwrap().count(), 0);
    }

    #[test]
    fn fetch_one_skips_when_stamp_matches() {
        let d = tmp("stamp");
        let zip_p = d.join("a.zip");
        make_zip(&zip_p);
        let hash = sha256_file(&zip_p).unwrap();
        std::fs::create_dir_all(d.join("deps-cache")).unwrap();
        std::fs::rename(&zip_p, d.join("deps-cache").join(&hash)).unwrap();
        let mut e = entry(&["root/lib/a.dll"], &[]);
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

    #[test]
    fn duplicate_destination_and_non_regular_are_errors() {
        let e = entry(&["a/x.dll", "b/x.dll"], &[]);
        assert!(dest_names(&e).unwrap_err().to_string().contains("x.dll"));

        let d = tmp("non_regular");
        let p = d.join("l.tgz");
        let gz = flate2::write::GzEncoder::new(
            std::fs::File::create(&p).unwrap(),
            flate2::Compression::fast(),
        );
        let mut t = tar::Builder::new(gz);
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Symlink);
        h.set_size(0);
        t.append_link(&mut h, "root/link", "/etc/passwd").unwrap();
        t.into_inner().unwrap().finish().unwrap();
        let err =
            extract_selected(&p, "u.tgz", &entry(&["root/link"], &[]), &d.join("o")).unwrap_err();
        assert!(err.to_string().contains("обычным файлом"), "{err}");
    }
}
