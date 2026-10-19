//! Загрузка, проверка SHA-256 и размера, извлечение файлов из архивов.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Write};
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

/// Сверяет размер файла с ожидаемым (`None` — размер не закреплён).
fn check_size(path: &Path, what: &str, expected: Option<u64>) -> Result<(), FetchError> {
    let Some(expected) = expected else {
        return Ok(());
    };
    let actual = fs::metadata(path)
        .map_err(FetchError::io(format!("размер {}", path.display())))?
        .len();
    if actual == expected {
        Ok(())
    } else {
        Err(FetchError::SizeMismatch {
            what: what.to_string(),
            expected,
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
    download_limited(url, dest, None)
}

/// [`download`] с пределом размера: тело длиннее `max` байт обрывает загрузку
/// (сервер отдаёт не тот файл — гигабайты впустую не качаются).
fn download_limited(url: &str, dest: &Path, max: Option<u64>) -> Result<(), FetchError> {
    let mut cfg = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        // крупные архивы (cuDNN — 1,3 ГБ) на медленном канале
        .timeout_global(Some(Duration::from_secs(4 * 3600)));
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
    let copy_err = |e: io::Error| FetchError::Http {
        url: url.to_string(),
        message: e.to_string(),
    };
    match max {
        None => {
            io::copy(&mut reader, &mut out).map_err(copy_err)?;
        }
        Some(max) => {
            let mut limited = reader.take(max.saturating_add(1));
            let n = io::copy(&mut limited, &mut out).map_err(copy_err)?;
            if n > max {
                return Err(FetchError::SizeMismatch {
                    what: url.to_string(),
                    expected: max,
                    actual: n,
                });
            }
        }
    }
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

/// Каталог и имя файла пути внутри архива (`a/b/c` → (`a/b`, `c`)).
fn split_dir(p: &str) -> (&str, &str) {
    p.rsplit_once('/').unwrap_or(("", p))
}

/// Цель символической ссылки в архиве: только простое имя в том же каталоге
/// (`libcudart.so.13 → libcudart.so.13.4.92`, в cuBLAS — `./libcublas.so.13.8.0.4`).
/// Иное (`../x`, `/etc/x`, `sub/x`) — `None`.
fn link_target(link_path: &str, target: &str) -> Option<String> {
    let mut t = target.trim();
    while let Some(rest) = t.strip_prefix("./") {
        t = rest;
    }
    if t.is_empty() || t == "." || t == ".." || t.contains(['/', '\\']) {
        return None;
    }
    let (dir, _) = split_dir(link_path);
    Some(if dir.is_empty() {
        t.to_string()
    } else {
        format!("{dir}/{t}")
    })
}

/// Сколько ссылок подряд разрешается (`libX.so.13 → libX.so.13.4 → libX.so.13.4.92`).
const MAX_LINK_HOPS: usize = 8;

/// Извлечение из tar-потока (tar.gz, tar.xz) за один проход.
///
/// Обычный файл из `wanted` пишется под своим именем назначения. Если в `wanted` указана
/// символическая ссылка (так в redist-архивах NVIDIA лежат имена SONAME:
/// `lib/libcudart.so.13 → libcudart.so.13.4.92`), под её именем кладётся файл, на который она
/// ведёт: только обычный файл того же каталога архива, имя которого начинается с имени
/// ссылки (такие файлы распаковываются во временные, лишние потом удаляются). Ссылки
/// наружу, жёсткие ссылки и прочие типы под нужными путями — ошибка.
fn extract_tar<R: Read>(
    reader: R,
    wanted: &[String],
    names: &[String],
    out: &Path,
    found: &mut [bool],
) -> Result<(), FetchError> {
    let out_ctx = || format!("запись в {}", out.display());
    // кандидат в цели ссылки: тот же каталог, имя начинается с имени нужного файла
    let is_candidate = |p: &str| {
        let (dir, name) = split_dir(p);
        wanted.iter().any(|w| {
            let (wd, wn) = split_dir(w);
            wd == dir && name.len() > wn.len() && name.starts_with(wn)
        })
    };
    // путь в архиве → (временный файл, права)
    let mut candidates: HashMap<String, (PathBuf, Option<u32>)> = HashMap::new();
    // путь ссылки → путь цели (только простые цели в том же каталоге)
    let mut links: HashMap<String, String> = HashMap::new();
    // номер временного файла кандидата (не число кандидатов: путь может повториться)
    let mut seq = 0usize;
    let mut t = tar::Archive::new(reader);
    let scan = (|| {
        for e in t.entries().map_err(archive_err)? {
            let mut e = e.map_err(archive_err)?;
            let p = norm(&e.path().map_err(archive_err)?.to_string_lossy());
            let kind = e.header().entry_type();
            let mode = e.header().mode().ok();
            let wanted_at = wanted.iter().position(|w| *w == p);
            if kind.is_symlink() {
                let target = e
                    .link_name()
                    .map_err(archive_err)?
                    .map(|t| t.to_string_lossy().into_owned())
                    .unwrap_or_default();
                match link_target(&p, &target) {
                    Some(t) => {
                        links.insert(p, t);
                    }
                    None if wanted_at.is_some() => {
                        return Err(FetchError::Invalid(format!(
                            "'{p}' в архиве — ссылка на '{target}' вне своего каталога"
                        )));
                    }
                    None => {}
                }
                continue;
            }
            if let Some(i) = wanted_at {
                if !kind.is_file() {
                    return Err(FetchError::Invalid(format!(
                        "'{p}' в архиве не является обычным файлом"
                    )));
                }
                let to = out.join(&names[i]);
                let mut o = File::create(&to).map_err(FetchError::io(out_ctx()))?;
                io::copy(&mut e, &mut o).map_err(archive_err)?;
                set_mode(&to, mode)?;
                found[i] = true;
            } else if kind.is_file() && is_candidate(&p) {
                seq += 1;
                let to = out.join(format!(".zam-link-{seq}"));
                let mut o = File::create(&to).map_err(FetchError::io(out_ctx()))?;
                // повтор пути в tar: действует последний, прежний временный файл удаляется
                if let Some((old, _)) = candidates.insert(p, (to, mode)) {
                    let _ = fs::remove_file(old);
                }
                io::copy(&mut e, &mut o).map_err(archive_err)?;
            }
        }
        Ok(())
    })();
    // ссылки из `wanted` → файлы, на которые они ведут
    let result = scan.and_then(|()| {
        for (i, w) in wanted.iter().enumerate() {
            if found[i] {
                continue;
            }
            let Some(mut cur) = links.get(w).cloned() else {
                continue;
            };
            let mut hops = 1;
            while let Some(next) = links.get(&cur) {
                hops += 1;
                if hops > MAX_LINK_HOPS {
                    return Err(FetchError::Invalid(format!(
                        "'{w}': слишком длинная цепочка ссылок"
                    )));
                }
                cur = next.clone();
            }
            let to = out.join(&names[i]);
            if let Some((src, mode)) = candidates.get(&cur) {
                fs::copy(src, &to).map_err(FetchError::io(out_ctx()))?;
                set_mode(&to, *mode)?;
            } else if let Some(j) = wanted.iter().position(|x| *x == cur).filter(|&j| found[j]) {
                fs::copy(out.join(&names[j]), &to).map_err(FetchError::io(out_ctx()))?;
            } else {
                return Err(FetchError::Invalid(format!(
                    "'{w}' — ссылка на '{cur}': такого обычного файла в каталоге архива нет \
                     (или его имя не начинается с имени ссылки)"
                )));
            }
            found[i] = true;
        }
        Ok(())
    });
    for (src, _) in candidates.values() {
        let _ = fs::remove_file(src);
    }
    result
}

/// Формат архива по расширению URL.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Zip,
    TarGz,
    TarXz,
}

fn format_of(url: &str) -> Option<Format> {
    let lower = url.to_ascii_lowercase();
    if lower.ends_with(".zip") {
        Some(Format::Zip)
    } else if lower.ends_with(".tgz") || lower.ends_with(".tar.gz") {
        Some(Format::TarGz)
    } else if lower.ends_with(".txz") || lower.ends_with(".tar.xz") {
        Some(Format::TarXz)
    } else {
        None
    }
}

/// Извлекает из архива (zip, tar.gz, tar.xz) только перечисленные в `entry.extract` файлы
/// в `out`. В tar под нужным путём допускается ссылка на файл того же каталога
/// (см. [`extract_tar`]); в zip — только обычные файлы.
pub fn extract_selected(
    archive: &Path,
    url: &str,
    entry: &PlatformEntry,
    out: &Path,
) -> Result<(), FetchError> {
    let wanted: Vec<String> = entry.extract.iter().map(|p| norm(p)).collect();
    // Имена назначения и формат проверяем до записи чего-либо.
    let names = dest_names(entry)?;
    let format = format_of(url)
        .ok_or_else(|| FetchError::Invalid(format!("неизвестный формат архива: {url}")))?;
    let out_ctx = || format!("запись в {}", out.display());
    fs::create_dir_all(out).map_err(FetchError::io(out_ctx()))?;
    let mut found = vec![false; wanted.len()];
    let open =
        || File::open(archive).map_err(FetchError::io(format!("открытие {}", archive.display())));
    match format {
        Format::Zip => {
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
        }
        Format::TarGz => {
            let gz = flate2::read::GzDecoder::new(BufReader::new(open()?));
            extract_tar(gz, &wanted, &names, out, &mut found)?;
        }
        Format::TarXz => {
            let xz = lzma_rust2::XzReader::new(BufReader::new(open()?), true);
            extract_tar(xz, &wanted, &names, out, &mut found)?;
        }
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

/// Временный файл загрузки `<out>.<pid>.part`. Старые `<out>.<число>.part` — следы
/// прерванных загрузок прошлых запусков — удаляются (одновременная загрузка одного файла
/// двумя процессами не поддерживается).
fn fresh_part(out: &Path) -> Result<PathBuf, FetchError> {
    let part = sibling(out, &format!(".{}.part", std::process::id()))?;
    let (Some(dir), Some(name)) = (out.parent(), out.file_name().and_then(|n| n.to_str())) else {
        return Ok(part);
    };
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    let prefix = format!("{name}.");
    let Ok(rd) = fs::read_dir(dir) else {
        return Ok(part);
    };
    for e in rd.flatten() {
        let n = e.file_name();
        let Some(n) = n.to_str() else { continue };
        let stale = n
            .strip_prefix(&prefix)
            .and_then(|r| r.strip_suffix(".part"))
            .is_some_and(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()));
        if stale && e.file_type().is_ok_and(|t| t.is_file()) {
            fs::remove_file(e.path())
                .map_err(FetchError::io(format!("удаление {}", e.path().display())))?;
        }
    }
    Ok(part)
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

/// Скачивает одиночный файл (без распаковки) в `dest` с проверкой размера (если задан) и
/// SHA-256. Загрузка идёт во временный `<имя>.<pid>.part` рядом с `dest`; при ошибке или
/// несовпадении он удаляется, `*.part` прерванных прошлых загрузок — тоже. Если `dest` уже
/// есть, а размер и хэш совпали — сеть не нужна, возвращается true; иначе файл удаляется
/// и скачивается заново.
pub fn fetch_file(
    url: &str,
    sha256: &str,
    size: Option<u64>,
    dest: &Path,
) -> Result<bool, FetchError> {
    let expected = sha256.trim().to_ascii_lowercase();
    if dest.is_file() {
        // размер — до хэша: чужой файл в гигабайт не хэшируется
        if check_size(dest, url, size).is_ok() && sha256_file(dest)? == expected {
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
    let part = fresh_part(dest)?;
    let checked = download_limited(url, &part, size).and_then(|()| {
        check_size(&part, url, size)?;
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

/// Штамп архива в каталоге установки: `<dir>/.<key>.sha256` с SHA-256 архива из манифеста
/// и переводом строки (тот же формат, что у штампа `<каталог>.sha256` в `cargo xtask
/// fetch-deps`). `key` — простое имя (компонент, `qdrant`).
pub fn stamp_path(dir: &Path, key: &str) -> Result<PathBuf, FetchError> {
    if key.is_empty()
        || key.starts_with('.')
        || !key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(FetchError::Invalid(format!(
            "недопустимый ключ штампа '{key}'"
        )));
    }
    Ok(dir.join(format!(".{key}.sha256")))
}

/// Установлен ли архив `entry` под ключом `key`: штамп совпал с хэшем манифеста и все
/// файлы `extract` на месте.
pub fn is_installed(entry: &PlatformEntry, dir: &Path, key: &str) -> Result<bool, FetchError> {
    let expected = entry.sha256.trim().to_ascii_lowercase();
    let stamp = stamp_path(dir, key)?;
    let ok = fs::read_to_string(&stamp)
        .is_ok_and(|t| !expected.is_empty() && t.trim().eq_ignore_ascii_case(&expected));
    Ok(ok && dest_names(entry)?.iter().all(|n| dir.join(n).is_file()))
}

/// Ставит файлы архива платформы в каталог `dir`, не трогая остальные файлы в нём
/// (например `vcruntime140.dll` рядом с `qdrant.exe` в комплекте Windows).
///
/// Пропуск (без сети, возвращается true) — только если штамп `.<key>.sha256` совпал с хэшем
/// манифеста и все файлы `extract` на месте ([`is_installed`]). Иначе (другой хэш в
/// манифесте, нет штампа — файлы поставлены не нами или прошлым выпуском) архив
/// скачивается во временный `<dir>.<pid>.part` рядом с каталогом (`*.part` прерванных
/// прошлых загрузок удаляются), размер (если задан) и хэш сверяются с манифестом, файлы
/// извлекаются в `<dir>.tmp` и заменяют прежние в `dir`, последним пишется штамп (до замены
/// старый штамп удаляется: прерванная замена не выглядит установленной); временные файлы
/// удаляются в любом исходе.
pub fn install_archive(entry: &PlatformEntry, dir: &Path, key: &str) -> Result<bool, FetchError> {
    let expected = entry.sha256.trim().to_ascii_lowercase();
    if expected.is_empty() {
        return Err(FetchError::Invalid(format!(
            "{}: sha256 не задан в манифесте",
            entry.url
        )));
    }
    let names = dest_names(entry)?;
    let stamp = stamp_path(dir, key)?;
    if is_installed(entry, dir, key)? {
        return Ok(true);
    }
    if let Some(parent) = dir.parent() {
        fs::create_dir_all(parent)
            .map_err(FetchError::io(format!("создание {}", parent.display())))?;
    }
    let part = fresh_part(dir)?;
    let tmp = sibling(dir, ".tmp")?;
    let result = (|| {
        download_limited(&entry.url, &part, entry.size)?;
        check_size(&part, &entry.url, entry.size)?;
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
        if stamp.exists() {
            fs::remove_file(&stamp)
                .map_err(FetchError::io(format!("удаление {}", stamp.display())))?;
        }
        for n in &names {
            let to = dir.join(n);
            if to.exists() {
                fs::remove_file(&to).map_err(FetchError::io(format!("замена {}", to.display())))?;
            }
            fs::rename(tmp.join(n), &to)
                .map_err(FetchError::io(format!("перенос в {}", to.display())))?;
        }
        let stamp_tmp = sibling(&stamp, ".part")?;
        fs::write(&stamp_tmp, format!("{expected}\n"))
            .and_then(|()| fs::rename(&stamp_tmp, &stamp))
            .map_err(FetchError::io(format!("запись {}", stamp.display())))?;
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
            size: None,
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
        assert!(!fetch_file(&url, HELLO_SHA, None, &dest).unwrap());
        assert_eq!(std::fs::read(&dest).unwrap(), b"hello");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        // повторный вызов: файл на месте и хэш совпал — без сети
        assert!(fetch_file(&url, HELLO_SHA, None, &dest).unwrap());
        // испорченный файл перекачивается (сервер уже закрыт — ошибка, файл удалён)
        std::fs::write(&dest, b"broken").unwrap();
        assert!(fetch_file(&url, HELLO_SHA, None, &dest).is_err());
        assert!(!dest.exists());
        assert_eq!(std::fs::read_dir(d.join("m")).unwrap().count(), 0);
    }

    #[test]
    fn fetch_file_sha_mismatch_removes_partial() {
        let d = tmp("fetch_file_bad_sha");
        let (url, _) = serve(b"hello", "file.bin", 1);
        let dest = d.join("m").join("model.onnx");
        let wrong = "0".repeat(64);
        let err = fetch_file(&url, &wrong, None, &dest)
            .unwrap_err()
            .to_string();
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
        assert!(!install_archive(&e, &dir, "q").unwrap());
        assert_eq!(std::fs::read(dir.join("a.dll")).unwrap(), b"AAA");
        assert_eq!(
            std::fs::read_to_string(dir.join(".q.sha256")).unwrap(),
            format!("{}\n", e.sha256)
        );
        assert_eq!(std::fs::read(dir.join("vcruntime140.dll")).unwrap(), b"VC");
        // временные файлы рядом с каталогом удалены
        let left: Vec<_> = std::fs::read_dir(d.join("qdrant"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, ["1.0.0"]);
        // штамп совпал и файлы на месте — без сети (сервер уже закрыт)
        assert!(install_archive(&e, &dir, "q").unwrap());
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn install_archive_replaces_on_new_hash_or_missing_stamp() {
        let d = tmp("install_stamp");
        let zip_p = d.join("src.zip");
        make_zip(&zip_p);
        let body: &'static [u8] = Box::leak(std::fs::read(&zip_p).unwrap().into_boxed_slice());
        let sha = sha256_file(&zip_p).unwrap();
        let dir = d.join("cuda");
        std::fs::create_dir_all(&dir).unwrap();
        // старые файлы без штампа (прошлый выпуск, ручная распаковка) — перекачка и замена
        std::fs::write(dir.join("a.dll"), b"OLD").unwrap();
        let (url, hits) = serve(body, "a.zip", 2);
        let mut e = entry(&["root/lib/a.dll"], &[]);
        e.url = url;
        e.sha256 = sha.clone();
        assert!(!is_installed(&e, &dir, "lib").unwrap());
        assert!(!install_archive(&e, &dir, "lib").unwrap());
        assert_eq!(std::fs::read(dir.join("a.dll")).unwrap(), b"AAA");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert!(is_installed(&e, &dir, "lib").unwrap());
        // другой хэш в манифесте (новый выпуск) — перекачка; здесь тело то же, хэш не сойдётся,
        // старые файлы и штамп остаются
        let mut e2 = entry(&["root/lib/a.dll"], &[]);
        e2.url = e.url.clone();
        e2.sha256 = "1".repeat(64);
        assert!(!is_installed(&e2, &dir, "lib").unwrap());
        assert!(matches!(
            install_archive(&e2, &dir, "lib"),
            Err(FetchError::Sha256Mismatch { .. })
        ));
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        assert_eq!(std::fs::read(dir.join("a.dll")).unwrap(), b"AAA");
        assert!(is_installed(&e, &dir, "lib").unwrap());
        // новый выпуск: другой архив и его хэш в манифесте — файлы и штамп заменяются
        let new_zip = d.join("new.zip");
        {
            let mut z = zip::ZipWriter::new(std::fs::File::create(&new_zip).unwrap());
            z.start_file("root/lib/a.dll", zip::write::SimpleFileOptions::default())
                .unwrap();
            z.write_all(b"NEW").unwrap();
            z.finish().unwrap();
        }
        let new_body: &'static [u8] =
            Box::leak(std::fs::read(&new_zip).unwrap().into_boxed_slice());
        let (url3, hits3) = serve(new_body, "a.zip", 1);
        let mut e3 = entry(&["root/lib/a.dll"], &[]);
        e3.url = url3;
        e3.sha256 = sha256_file(&new_zip).unwrap();
        assert!(!install_archive(&e3, &dir, "lib").unwrap());
        assert_eq!(hits3.load(Ordering::SeqCst), 1);
        assert_eq!(std::fs::read(dir.join("a.dll")).unwrap(), b"NEW");
        assert!(is_installed(&e3, &dir, "lib").unwrap() && !is_installed(&e, &dir, "lib").unwrap());
        // совпал — без сети (сервер закрыт)
        assert!(install_archive(&e3, &dir, "lib").unwrap());
        // штамп есть, а файла нет — не установлено
        std::fs::remove_file(dir.join("a.dll")).unwrap();
        assert!(!is_installed(&e, &dir, "lib").unwrap());
        assert!(stamp_path(&dir, "../x").is_err() && stamp_path(&dir, "").is_err());
    }

    #[test]
    fn tar_duplicate_candidate_leaves_no_temp_files() {
        let d = tmp("txz_dup");
        let p = d.join("a.tar.xz");
        make_txz(
            &p,
            &[
                T::File("a/lib/libq.so.1.0", "OLD", 0o644),
                T::File("a/lib/libq.so.1.0", "NEW", 0o644),
                T::Link("a/lib/libq.so.1", "libq.so.1.0"),
            ],
        );
        let out = d.join("out");
        extract_selected(&p, "u.tar.xz", &entry(&["a/lib/libq.so.1"], &[]), &out).unwrap();
        assert_eq!(std::fs::read(out.join("libq.so.1")).unwrap(), b"NEW");
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 1);
    }

    #[test]
    fn install_archive_sha_mismatch_leaves_nothing() {
        let d = tmp("install_archive_bad");
        let (url, _) = serve(b"hello", "q.zip", 1);
        let mut e = entry(&["root/lib/a.dll"], &[]);
        e.url = url;
        e.sha256 = "0".repeat(64);
        let dir = d.join("qdrant").join("1.0.0");
        let err = install_archive(&e, &dir, "q").unwrap_err().to_string();
        assert!(err.contains(HELLO_SHA), "{err}");
        assert_eq!(std::fs::read_dir(d.join("qdrant")).unwrap().count(), 0);
        e.sha256.clear();
        assert!(install_archive(&e, &dir, "q").is_err());
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
        assert!(err.to_string().contains("вне своего каталога"), "{err}");

        // жёсткая ссылка под нужным путём — не обычный файл
        let p = d.join("h.tgz");
        let gz = flate2::write::GzEncoder::new(
            std::fs::File::create(&p).unwrap(),
            flate2::Compression::fast(),
        );
        let mut t = tar::Builder::new(gz);
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Link);
        h.set_size(0);
        t.append_link(&mut h, "root/hard", "root/other").unwrap();
        t.into_inner().unwrap().finish().unwrap();
        let err =
            extract_selected(&p, "u.tgz", &entry(&["root/hard"], &[]), &d.join("o2")).unwrap_err();
        assert!(err.to_string().contains("обычным файлом"), "{err}");
    }

    /// Элемент tar для [`make_txz`]: обычный файл (путь, содержимое, права) или ссылка.
    enum T<'a> {
        File(&'a str, &'a str, u32),
        Link(&'a str, &'a str),
    }

    /// tar.xz в `path` из элементов `items` (в заданном порядке).
    fn make_txz(path: &Path, items: &[T]) {
        let mut t = tar::Builder::new(Vec::new());
        for it in items {
            let mut h = tar::Header::new_gnu();
            match it {
                T::File(n, c, mode) => {
                    h.set_size(c.len() as u64);
                    h.set_mode(*mode);
                    t.append_data(&mut h, n, c.as_bytes()).unwrap();
                }
                T::Link(n, target) => {
                    h.set_entry_type(tar::EntryType::Symlink);
                    h.set_size(0);
                    t.append_link(&mut h, n, target).unwrap();
                }
            }
        }
        let tar_bytes = t.into_inner().unwrap();
        let f = std::fs::File::create(path).unwrap();
        let mut w = lzma_rust2::XzWriter::new(f, lzma_rust2::XzOptions::with_preset(1)).unwrap();
        w.write_all(&tar_bytes).unwrap();
        w.finish().unwrap();
    }

    #[test]
    fn tar_xz_soname_links_resolve_to_files_of_same_dir() {
        let d = tmp("txz_links");
        let p = d.join("cuda.tar.xz");
        make_txz(
            &p,
            &[
                // ссылка раньше своей цели и после неё
                T::Link("a/lib/libx.so.13", "libx.so.13.4.92"),
                T::File("a/lib/libx.so.13.4.92", "XXX", 0o755),
                T::File("a/lib/liby.so.1.0.5", "YYY", 0o644),
                T::Link("a/lib/liby.so.1.0", "liby.so.1.0.5"),
                T::Link("a/lib/liby.so.1", "./liby.so.1.0"),
                T::File("a/lib/libx_static.a", "big", 0o644),
                T::File("a/lib/libplain.so.2", "PPP", 0o644),
            ],
        );
        let e = entry(
            &["a/lib/libx.so.13", "a/lib/liby.so.1", "a/lib/libplain.so.2"],
            &[],
        );
        let out = d.join("out");
        extract_selected(&p, "https://x/cuda.tar.xz", &e, &out).unwrap();
        assert_eq!(std::fs::read(out.join("libx.so.13")).unwrap(), b"XXX");
        assert_eq!(std::fs::read(out.join("liby.so.1")).unwrap(), b"YYY");
        assert_eq!(std::fs::read(out.join("libplain.so.2")).unwrap(), b"PPP");
        // только нужные имена: ни целей ссылок, ни временных файлов
        let mut left: Vec<String> = std::fs::read_dir(&out)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["libplain.so.2", "libx.so.13", "liby.so.1"]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode =
                |n: &str| std::fs::metadata(out.join(n)).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode("libx.so.13"), 0o755);
            assert_eq!(mode("liby.so.1"), 0o644);
            // ссылка не создаётся: под именем SONAME лежит обычный файл
            assert!(
                !std::fs::symlink_metadata(out.join("libx.so.13"))
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
        }
    }

    #[test]
    fn tar_xz_bad_links_are_errors() {
        let d = tmp("txz_bad_links");
        let cases: [(&str, &[T], &str); 4] = [
            // цель вне каталога
            (
                "up",
                &[T::Link("a/lib/libz.so.1", "../evil.so.1.0")],
                "вне своего каталога",
            ),
            (
                "abs",
                &[T::Link("a/lib/libz.so.1", "/usr/lib/libz.so.1.0")],
                "вне своего каталога",
            ),
            // цель — файл с чужим именем (не кандидат)
            (
                "other",
                &[
                    T::Link("a/lib/libz.so.1", "other.so"),
                    T::File("a/lib/other.so", "O", 0o644),
                ],
                "a/lib/other.so",
            ),
            // цели нет в архиве
            (
                "dangling",
                &[T::Link("a/lib/libz.so.1", "libz.so.1.2")],
                "a/lib/libz.so.1.2",
            ),
        ];
        for (name, items, needle) in cases {
            let p = d.join(format!("{name}.tar.xz"));
            make_txz(&p, items);
            let out = d.join(format!("out-{name}"));
            let err = extract_selected(&p, "u.tar.xz", &entry(&["a/lib/libz.so.1"], &[]), &out)
                .unwrap_err()
                .to_string();
            assert!(err.contains(needle), "{name}: {err}");
            // временные кандидаты удалены
            let n = std::fs::read_dir(&out).map_or(0, |r| r.count());
            assert_eq!(n, 0, "{name}");
        }
    }

    #[test]
    fn fetch_file_checks_size_and_removes_stale_parts() {
        let d = tmp("fetch_file_size");
        let dest = d.join("m").join("model.onnx");
        std::fs::create_dir_all(d.join("m")).unwrap();
        // следы прерванных загрузок прошлых запусков; чужие файлы не трогаются
        std::fs::write(d.join("m").join("model.onnx.4242.part"), b"old").unwrap();
        std::fs::write(d.join("m").join("model.onnx.x.part"), b"keep").unwrap();
        std::fs::write(d.join("m").join("tokenizer.json.77.part"), b"keep").unwrap();
        // тело длиннее закреплённого размера — загрузка обрывается
        let (url, _) = serve(b"hello", "file.bin", 1);
        let err = fetch_file(&url, HELLO_SHA, Some(3), &dest).unwrap_err();
        assert!(matches!(err, FetchError::SizeMismatch { .. }), "{err}");
        // короче закреплённого — ошибка до хэша
        let (url, _) = serve(b"hello", "file.bin", 1);
        let err = fetch_file(&url, HELLO_SHA, Some(10), &dest).unwrap_err();
        assert!(err.to_string().contains("10"), "{err}");
        assert!(!dest.exists());
        let (url, _) = serve(b"hello", "file.bin", 1);
        assert!(!fetch_file(&url, HELLO_SHA, Some(5), &dest).unwrap());
        let mut left: Vec<String> = std::fs::read_dir(d.join("m"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            ["model.onnx", "model.onnx.x.part", "tokenizer.json.77.part"]
        );
        // на месте, но другого размера — перекачивается (сервер закрыт — ошибка, файл удалён)
        assert!(fetch_file(&url, HELLO_SHA, Some(6), &dest).is_err());
        assert!(!dest.exists());
    }
}
