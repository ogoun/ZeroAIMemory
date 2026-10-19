//! Загрузка, проверка SHA-256 и извлечение файлов из архивов.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::{FetchError, PlatformEntry};

/// SHA-256 файла в нижнем регистре (hex).
pub fn sha256_file(path: &Path) -> Result<String, FetchError> {
    let ctx = || format!("не удалось прочитать {}", path.display());
    let mut f = File::open(path).map_err(FetchError::io(ctx()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).map_err(FetchError::io(ctx()))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Сверяет хэш файла с ожидаемым (регистр не важен).
pub fn verify_sha256(path: &Path, expected: &str) -> Result<(), FetchError> {
    let actual = sha256_file(path)?;
    if actual.eq_ignore_ascii_case(expected.trim()) {
        Ok(())
    } else {
        Err(FetchError::Sha256Mismatch {
            what: path.display().to_string(),
            expected: expected.trim().to_string(),
            actual,
        })
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
/// При ошибке частичный `dest` остаётся — его удаляет вызывающий.
pub fn download(url: &str, dest: &Path) -> Result<(), FetchError> {
    let mut cfg = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        // крупные модели (до 1,2 ГБ) на медленном канале
        .timeout_global(Some(Duration::from_secs(3600)));
    if is_loopback(url) {
        cfg = cfg.proxy(None);
    }
    let agent: ureq::Agent = cfg.build().into();
    let http = |e: ureq::Error| FetchError::Http {
        url: url.to_string(),
        message: e.to_string(),
    };
    let resp = agent.get(url).call().map_err(http)?;
    let mut reader = resp.into_body().into_reader();
    let mut out = File::create(dest).map_err(FetchError::io(format!(
        "не удалось создать {}",
        dest.display()
    )))?;
    io::copy(&mut reader, &mut out).map_err(|e| FetchError::Http {
        url: url.to_string(),
        message: e.to_string(),
    })?;
    out.flush()
        .map_err(FetchError::io(format!("запись {}", dest.display())))?;
    Ok(())
}

/// Путь внутри архива без `./` и с прямыми слэшами.
fn norm(p: &str) -> String {
    p.replace('\\', "/").trim_start_matches("./").to_string()
}

/// Имя файла назначения: rename или последний компонент пути; только простое имя.
fn dest_name(entry: &PlatformEntry, path: &str) -> Result<String, FetchError> {
    let name = match entry.rename.get(path) {
        Some(n) => n.clone(),
        None => path.rsplit('/').next().unwrap_or(path).to_string(),
    };
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', ':']) {
        return Err(FetchError::Invalid(format!(
            "недопустимое имя назначения '{name}' для '{path}'"
        )));
    }
    Ok(name)
}

/// Имена назначения для всех `extract`; дубликаты и недопустимые имена — ошибка.
pub fn dest_names(entry: &PlatformEntry) -> Result<Vec<String>, FetchError> {
    let mut names: Vec<String> = Vec::new();
    for p in &entry.extract {
        let n = dest_name(entry, &norm(p))?;
        if names.contains(&n) {
            return Err(FetchError::Invalid(format!(
                "два файла извлекаются под одним именем '{n}'"
            )));
        }
        names.push(n);
    }
    Ok(names)
}

fn archive_err(e: impl std::fmt::Display) -> FetchError {
    FetchError::Invalid(format!("ошибка чтения архива: {e}"))
}

/// Права извлечённого файла на Unix: исполняемый в архиве (любой бит x) → 0o755,
/// иначе 0o644. Без этого бинарник Qdrant из tar.gz не запускается (EACCES).
/// На Windows права не применяются.
fn set_mode(path: &Path, mode: Option<u32>) -> Result<(), FetchError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let m = if mode.is_some_and(|m| m & 0o111 != 0) {
            0o755
        } else {
            0o644
        };
        fs::set_permissions(path, fs::Permissions::from_mode(m))
            .map_err(FetchError::io(format!("права на {}", path.display())))?;
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}

/// Извлекает из архива только перечисленные в `entry.extract` обычные файлы в `out`.
pub fn extract_selected(
    archive: &Path,
    url: &str,
    entry: &PlatformEntry,
    out: &Path,
) -> Result<(), FetchError> {
    let wanted: Vec<String> = entry.extract.iter().map(|p| norm(p)).collect();
    // Имена назначения проверяем до записи чего-либо.
    let names = dest_names(entry)?;
    let out_ctx = || format!("запись в {}", out.display());
    fs::create_dir_all(out).map_err(FetchError::io(out_ctx()))?;
    let mut found = vec![false; wanted.len()];
    let open =
        || File::open(archive).map_err(FetchError::io(format!("открытие {}", archive.display())));
    let lower = url.to_ascii_lowercase();
    if lower.ends_with(".zip") {
        let mut z = zip::ZipArchive::new(open()?)
            .map_err(|e| FetchError::Invalid(format!("не zip-архив: {e}")))?;
        for (i, w) in wanted.iter().enumerate() {
            let mut f = match z.by_name(w) {
                Ok(f) => f,
                Err(zip::result::ZipError::FileNotFound) => continue,
                Err(e) => {
                    return Err(FetchError::Invalid(format!(
                        "ошибка чтения '{w}' из zip: {e}"
                    )));
                }
            };
            if !f.is_file() || f.is_symlink() {
                return Err(FetchError::Invalid(format!(
                    "'{w}' в архиве не является обычным файлом"
                )));
            }
            let to = out.join(&names[i]);
            let mut o = File::create(&to).map_err(FetchError::io(out_ctx()))?;
            io::copy(&mut f, &mut o).map_err(archive_err)?;
            set_mode(&to, f.unix_mode())?;
            found[i] = true;
        }
    } else if lower.ends_with(".tgz") || lower.ends_with(".tar.gz") {
        let gz = flate2::read::GzDecoder::new(open()?);
        let mut t = tar::Archive::new(gz);
        for e in t.entries().map_err(archive_err)? {
            let mut e = e.map_err(archive_err)?;
            let p = norm(&e.path().map_err(archive_err)?.to_string_lossy());
            if let Some(i) = wanted.iter().position(|w| *w == p) {
                if !e.header().entry_type().is_file() {
                    return Err(FetchError::Invalid(format!(
                        "'{p}' в архиве не является обычным файлом"
                    )));
                }
                let to = out.join(&names[i]);
                let mode = e.header().mode().ok();
                let mut o = File::create(&to).map_err(FetchError::io(out_ctx()))?;
                io::copy(&mut e, &mut o).map_err(archive_err)?;
                set_mode(&to, mode)?;
                found[i] = true;
            }
        }
    } else {
        return Err(FetchError::Invalid(format!(
            "неизвестный формат архива: {url}"
        )));
    }
    let missing: Vec<&str> = wanted
        .iter()
        .zip(&found)
        .filter(|(_, f)| !**f)
        .map(|(w, _)| w.as_str())
        .collect();
    if !missing.is_empty() {
        return Err(FetchError::Invalid(format!(
            "в архиве не найдены файлы: {}",
            missing.join(", ")
        )));
    }
    Ok(())
}

/// Соседний путь `<out><suffix>` (для `*.tmp`, `*.part`).
fn sibling(out: &Path, suffix: &str) -> Result<PathBuf, FetchError> {
    let mut name = out
        .file_name()
        .ok_or_else(|| FetchError::Invalid(format!("пустое имя пути {}", out.display())))?
        .to_os_string();
    name.push(suffix);
    Ok(out.with_file_name(name))
}

/// Атомарная замена каталога: распаковка в `<out>.tmp`, затем подмена `out`.
pub fn extract_atomic(
    archive: &Path,
    url: &str,
    entry: &PlatformEntry,
    out: &Path,
) -> Result<(), FetchError> {
    let tmp = sibling(out, ".tmp")?;
    let ctx = || format!("замена каталога {}", out.display());
    if tmp.exists() {
        fs::remove_dir_all(&tmp).map_err(FetchError::io(ctx()))?;
    }
    if let Err(e) = extract_selected(archive, url, entry, &tmp) {
        let _ = fs::remove_dir_all(&tmp);
        return Err(e);
    }
    if out.exists() {
        fs::remove_dir_all(out).map_err(FetchError::io(ctx()))?;
    }
    fs::rename(&tmp, out).map_err(FetchError::io(ctx()))?;
    Ok(())
}

/// Скачивает одиночный файл (без распаковки) в `dest` с проверкой SHA-256.
/// Загрузка идёт во временный `<имя>.<pid>.part` рядом с `dest`; при ошибке или
/// несовпадении хэша он удаляется. Если `dest` уже есть и хэш совпал — сеть не нужна,
/// возвращается true; файл с чужим хэшем удаляется и скачивается заново.
pub fn fetch_file(url: &str, sha256: &str, dest: &Path) -> Result<bool, FetchError> {
    let expected = sha256.trim().to_ascii_lowercase();
    if dest.is_file() {
        if sha256_file(dest)? == expected {
            return Ok(true);
        }
        fs::remove_file(dest).map_err(FetchError::io(format!(
            "не удалось удалить {}",
            dest.display()
        )))?;
    }
    let dir = dest
        .parent()
        .ok_or_else(|| FetchError::Invalid("у пути назначения нет каталога".into()))?;
    fs::create_dir_all(dir).map_err(FetchError::io(format!("создание {}", dir.display())))?;
    let part = sibling(dest, &format!(".{}.part", std::process::id()))?;
    let checked = download(url, &part).and_then(|()| {
        let actual = sha256_file(&part)?;
        if actual == expected {
            Ok(())
        } else {
            Err(FetchError::Sha256Mismatch {
                what: url.to_string(),
                expected: expected.clone(),
                actual,
            })
        }
    });
    if let Err(e) = checked {
        let _ = fs::remove_file(&part);
        return Err(e);
    }
    fs::rename(&part, dest).map_err(FetchError::io(format!(
        "не удалось переименовать в {}",
        dest.display()
    )))?;
    Ok(false)
}

/// Ставит файлы архива платформы в каталог `dir`, не трогая остальные файлы в нём
/// (например `vcruntime140.dll` рядом с `qdrant.exe` в комплекте Windows).
///
/// Если все файлы `extract` уже на месте — сеть не нужна, возвращается true.
/// Иначе архив скачивается во временный `<dir>.<pid>.part` рядом с каталогом, хэш
/// сверяется с манифестом, файлы извлекаются в `<dir>.tmp` и переносятся в `dir`;
/// временные файлы удаляются в любом исходе.
pub fn install_archive(entry: &PlatformEntry, dir: &Path) -> Result<bool, FetchError> {
    let expected = entry.sha256.trim().to_ascii_lowercase();
    if expected.is_empty() {
        return Err(FetchError::Invalid(format!(
            "{}: sha256 не задан в манифесте",
            entry.url
        )));
    }
    let names = dest_names(entry)?;
    if names.iter().all(|n| dir.join(n).is_file()) {
        return Ok(true);
    }
    if let Some(parent) = dir.parent() {
        fs::create_dir_all(parent)
            .map_err(FetchError::io(format!("создание {}", parent.display())))?;
    }
    let part = sibling(dir, &format!(".{}.part", std::process::id()))?;
    let tmp = sibling(dir, ".tmp")?;
    let result = (|| {
        download(&entry.url, &part)?;
        let actual = sha256_file(&part)?;
        if actual != expected {
            return Err(FetchError::Sha256Mismatch {
                what: entry.url.clone(),
                expected: expected.clone(),
                actual,
            });
        }
        if tmp.exists() {
            fs::remove_dir_all(&tmp)
                .map_err(FetchError::io(format!("удаление {}", tmp.display())))?;
        }
        extract_selected(&part, &entry.url, entry, &tmp)?;
        fs::create_dir_all(dir).map_err(FetchError::io(format!("создание {}", dir.display())))?;
        for n in &names {
            let to = dir.join(n);
            if to.exists() {
                fs::remove_file(&to).map_err(FetchError::io(format!("замена {}", to.display())))?;
            }
            fs::rename(tmp.join(n), &to)
                .map_err(FetchError::io(format!("перенос в {}", to.display())))?;
        }
        Ok(())
    })();
    let _ = fs::remove_file(&part);
    let _ = fs::remove_dir_all(&tmp);
    result.map(|()| false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

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
            ("root/bin/tool", "#!"),
        ] {
            let mut h = tar::Header::new_gnu();
            h.set_size(c.len() as u64);
            h.set_mode(if n.ends_with("tool") { 0o755 } else { 0o644 });
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
        assert_eq!(actual, HELLO_SHA);
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

    #[cfg(unix)]
    #[test]
    fn executable_bit_survives_extraction() {
        use std::os::unix::fs::PermissionsExt as _;
        let d = tmp("extract_mode");
        let tgz_p = d.join("a.tgz");
        make_tgz(&tgz_p);
        let out = d.join("out");
        let e = entry(&["root/bin/tool", "root/lib/b.so"], &[]);
        extract_selected(&tgz_p, "https://x/a.tgz", &e, &out).unwrap();
        let mode = |n: &str| std::fs::metadata(out.join(n)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode("tool"), 0o755);
        assert_eq!(mode("b.so"), 0o644);
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

    /// Локальный HTTP-сервер: отдаёт `body` на каждый из `n` запросов, затем закрывается.
    /// Возвращает URL и счётчик обслуженных запросов.
    fn serve(body: &'static [u8], name: &str, n: usize) -> (String, std::sync::Arc<AtomicUsize>) {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/{name}", l.local_addr().unwrap());
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
        let (url, hits) = serve(b"hello", "file.bin", 1);
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
        let (url, _) = serve(b"hello", "file.bin", 1);
        let dest = d.join("m").join("model.onnx");
        let wrong = "0".repeat(64);
        let err = fetch_file(&url, &wrong, &dest).unwrap_err().to_string();
        assert!(err.contains(&wrong) && err.contains(HELLO_SHA), "{err}");
        assert!(!dest.exists());
        // временный *.part удалён, каталог пуст
        assert_eq!(std::fs::read_dir(d.join("m")).unwrap().count(), 0);
    }

    #[test]
    fn install_archive_keeps_other_files_and_skips_when_present() {
        let d = tmp("install_archive");
        let zip_p = d.join("src.zip");
        make_zip(&zip_p);
        let body: &'static [u8] = Box::leak(std::fs::read(&zip_p).unwrap().into_boxed_slice());
        let (url, hits) = serve(body, "q.zip", 1);
        let mut e = entry(&["root/lib/a.dll"], &[]);
        e.url = url;
        e.sha256 = sha256_file(&zip_p).unwrap();
        let dir = d.join("qdrant").join("1.0.0");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("vcruntime140.dll"), b"VC").unwrap();
        assert!(!install_archive(&e, &dir).unwrap());
        assert_eq!(std::fs::read(dir.join("a.dll")).unwrap(), b"AAA");
        assert_eq!(std::fs::read(dir.join("vcruntime140.dll")).unwrap(), b"VC");
        // временные файлы рядом с каталогом удалены
        let left: Vec<_> = std::fs::read_dir(d.join("qdrant"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, ["1.0.0"]);
        // файлы на месте — без сети (сервер уже закрыт)
        assert!(install_archive(&e, &dir).unwrap());
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn install_archive_sha_mismatch_leaves_nothing() {
        let d = tmp("install_archive_bad");
        let (url, _) = serve(b"hello", "q.zip", 1);
        let mut e = entry(&["root/lib/a.dll"], &[]);
        e.url = url;
        e.sha256 = "0".repeat(64);
        let dir = d.join("qdrant").join("1.0.0");
        let err = install_archive(&e, &dir).unwrap_err().to_string();
        assert!(err.contains(HELLO_SHA), "{err}");
        assert_eq!(std::fs::read_dir(d.join("qdrant")).unwrap().count(), 0);
        e.sha256.clear();
        assert!(install_archive(&e, &dir).is_err());
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
