//! Кэш архивов `target/deps-cache` и штампы распакованных зависимостей.
//! Загрузка, проверка размера и SHA-256, извлечение — в общем крейте `zam-fetch` (их же
//! использует сервер).

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use zam_fetch::{PlatformEntry, download, extract_atomic, verify_sha256};
pub use zam_fetch::{dest_names, sha256_file};

/// Скачивает во временный `*.part` внутри кэша с проверкой размера `size` (если задан);
/// возвращает путь и хэш. При ошибке `*.part` удаляется.
fn download_part(url: &str, cache: &Path, size: Option<u64>) -> Result<(PathBuf, String)> {
    fs::create_dir_all(cache)?;
    let part = cache.join(format!("{}.part", std::process::id()));
    if let Err(e) = download(url, &part, size) {
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

/// Скачивает (если нет в кэше), проверяет размер (`size` манифеста, если задан) и хэш и
/// распаковывает. `root` — каталог `target/`. Возвращает true, если загрузка не понадобилась
/// (архив в кэше или каталог уже распакован из того же архива — по штампу `<out>.sha256`; так
/// CI достаточно кэшировать `target/deps`).
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
        // размер — до хэша: чужой файл не хэшируется
        let len = fs::metadata(&cached)?.len();
        if let Some(size) = entry.size
            && len != size
        {
            bail!(
                "{name}: файл кэша {} — {len} байт, а size в манифесте — {size}; исправьте size \
                 в манифесте или удалите файл кэша и повторите",
                cached.display()
            );
        }
        verify_sha256(&cached, &expected).with_context(|| {
            format!(
                "{name}: файл кэша повреждён; удалите файл кэша {} и повторите",
                cached.display()
            )
        })?;
    } else {
        println!("{name}: загрузка {}", entry.url);
        let (part, actual) = download_part(&entry.url, &cache, entry.size)?;
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

/// Скачивает архив (без проверки размера: значения манифеста могут быть прежней версии), кладёт
/// в кэш под фактическим хэшем и возвращает хэш и размер.
pub fn print_hash(url: &str, root: &Path) -> Result<(String, u64)> {
    let cache = root.join("deps-cache");
    let (part, hash) = download_part(url, &cache, None)?;
    let size = fs::metadata(&part)?.len();
    fs::rename(&part, cache.join(&hash))?;
    Ok((hash, size))
}

/// sha256 всех манифестов `deploy/*.toml` (зависимости, `cuda.toml`, `models.toml`): значения
/// ключей `sha256` на любой глубине — ключ общий для всех схем, поэтому новый манифест не
/// требует правки здесь. Нечитаемый манифест — ошибка: чистка по неполному списку удалила бы
/// нужный архив.
pub fn manifest_hashes(deploy: &Path) -> Result<BTreeSet<String>> {
    fn walk(t: &toml::Table, out: &mut BTreeSet<String>) {
        for (k, v) in t {
            match v {
                toml::Value::String(s) if k == "sha256" && !s.trim().is_empty() => {
                    out.insert(s.trim().to_ascii_lowercase());
                }
                toml::Value::Table(t) => walk(t, out),
                toml::Value::Array(a) => {
                    for t in a.iter().filter_map(toml::Value::as_table) {
                        walk(t, out);
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = BTreeSet::new();
    let rd = fs::read_dir(deploy).with_context(|| format!("чтение {}", deploy.display()))?;
    for e in rd {
        let p = e?.path();
        if p.extension().is_none_or(|x| x != "toml") || !p.is_file() {
            continue;
        }
        let text = fs::read_to_string(&p).with_context(|| format!("чтение {}", p.display()))?;
        let t: toml::Table = text
            .parse()
            .with_context(|| format!("разбор {}", p.display()))?;
        walk(&t, &mut out);
    }
    Ok(out)
}

/// Удаляет из каталога кэша архивов `cache` файлы, имени которых нет в `keep` (sha256 архивов
/// текущих манифестов, [`manifest_hashes`]): прежние версии и `*.part` прерванных загрузок.
/// Подкаталоги не трогаются; нет каталога — удалять нечего. Возвращает число и объём (байт)
/// удалённых файлов.
pub fn prune_cache(cache: &Path, keep: &BTreeSet<String>) -> Result<(usize, u64)> {
    let rd = match fs::read_dir(cache) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok((0, 0)),
        Err(e) => return Err(e).with_context(|| format!("чтение {}", cache.display())),
    };
    let (mut n, mut bytes) = (0, 0);
    for e in rd {
        let e = e?;
        if !e.file_type()?.is_file()
            || keep.contains(&e.file_name().to_string_lossy().to_ascii_lowercase())
        {
            continue;
        }
        let len = e.metadata()?.len();
        fs::remove_file(e.path()).with_context(|| format!("удаление {}", e.path().display()))?;
        n += 1;
        bytes += len;
    }
    Ok((n, bytes))
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

    /// M7.1 (§4.1 п. 3): `size` манифеста сверяется и для архива из кэша — до хэша и
    /// распаковки.
    #[test]
    fn fetch_rejects_size_mismatch() {
        let d = tmp("size_cache");
        let zip_p = d.join("a.zip");
        make_zip(&zip_p);
        let hash = sha256_file(&zip_p).unwrap();
        let len = std::fs::metadata(&zip_p).unwrap().len();
        std::fs::create_dir_all(d.join("deps-cache")).unwrap();
        std::fs::rename(&zip_p, d.join("deps-cache").join(&hash)).unwrap();
        let mut e = entry(&["root/lib/a.dll"]);
        e.sha256 = hash;
        e.url = "https://x/a.zip".into();
        e.size = Some(len + 1);
        let out = d.join("deps").join("dep");
        let err = format!("{:#}", fetch_one("dep", &e, &d, &out).unwrap_err());
        assert!(
            err.contains("size") && err.contains(&len.to_string()),
            "{err}"
        );
        assert!(!out.exists() && !d.join("deps").join("dep.sha256").exists());
        // верный size — распаковка из кэша
        e.size = Some(len);
        assert!(fetch_one("dep", &e, &d, &out).unwrap());
        assert_eq!(std::fs::read(out.join("a.dll")).unwrap(), b"AAA");
    }

    /// Отдаёт `body` на один HTTP-запрос с локальной петли; возвращает URL `…/<name>`.
    fn serve_once(body: &'static [u8], name: &str) -> String {
        use std::io::Read as _;
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/{name}", l.local_addr().unwrap());
        std::thread::spawn(move || {
            let Ok((mut s, _)) = l.accept() else { return };
            let mut req = Vec::new();
            let mut b = [0u8; 1024];
            while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                match s.read(&mut b) {
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
        });
        url
    }

    /// Загрузка длиннее `size` манифеста обрывается; в кэше не остаётся ни архива, ни `*.part`.
    #[test]
    fn fetch_download_rejects_size_mismatch() {
        let d = tmp("size_download");
        let body: &'static [u8] = b"0123456789";
        let mut e = entry(&["root/lib/a.dll"]);
        e.url = serve_once(body, "a.nupkg");
        e.sha256 = {
            let p = d.join("body");
            std::fs::write(&p, body).unwrap();
            sha256_file(&p).unwrap()
        };
        e.size = Some(4);
        let err = format!(
            "{:#}",
            fetch_one("dep", &e, &d, &d.join("out")).unwrap_err()
        );
        assert!(
            err.contains("размер") && err.contains("больше 4 байт"),
            "{err}"
        );
        assert_eq!(std::fs::read_dir(d.join("deps-cache")).unwrap().count(), 0);
    }

    /// M7.1 (§4.1 п. 4): `--prune-cache` оставляет в `deps-cache` только архивы, sha256 которых
    /// есть в манифестах `deploy/*.toml` (зависимости и `[[lib]]` cuda — на любой глубине).
    #[test]
    fn prune_cache_keeps_manifest_hashes() {
        let d = tmp("prune");
        let (dep, lib, old) = ("a".repeat(64), "b".repeat(64), "c".repeat(64));
        let deploy = d.join("deploy");
        std::fs::create_dir_all(&deploy).unwrap();
        std::fs::write(
            deploy.join("dep.toml"),
            format!(
                "name = \"dep\"\nversion = \"1\"\n[platform.windows-x64]\nurl = \"https://x/a.zip\"\n\
                 sha256 = \"{}\"\nextract = [\"a\"]\n",
                dep.to_uppercase()
            ),
        )
        .unwrap();
        std::fs::write(
            deploy.join("cuda.toml"),
            format!("cuda = \"13\"\n[[lib]]\nsha256 = \"{lib}\"\n"),
        )
        .unwrap();
        let cache = d.join("deps-cache");
        std::fs::create_dir_all(&cache).unwrap();
        for (n, body) in [(&dep, "AAA"), (&lib, "BB"), (&old, "OLDOLD")] {
            std::fs::write(cache.join(n), body).unwrap();
        }
        let keep = manifest_hashes(&deploy).unwrap();
        assert_eq!(prune_cache(&cache, &keep).unwrap(), (1, 6));
        let mut left: Vec<String> = std::fs::read_dir(&cache)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, [dep, lib]);
        // повторно удалять нечего; нет каталога кэша — не ошибка
        assert_eq!(prune_cache(&cache, &keep).unwrap(), (0, 0));
        assert_eq!(prune_cache(&d.join("nope"), &keep).unwrap(), (0, 0));
    }
}
