//! Перенос пространства (M6 §12б.5): `zam export [--space S] [--path P]… [--kind K]…
//! [--no-history] -o <файл.zip>` — архив `GET /spaces/{space}/export` в файл. Импорт — 18b2.
//!
//! - Пространство: `--space` (`rules` | `human` | `knowledge` | `solution` | GUID | id), иначе
//!   решение из `solution.toml`, иначе `knowledge` — как у адресов заметок.
//! - Архив пишется потоком в новый `<файл>.<pid>.part` рядом с целью (`create_new`: чужой файл не
//!   трогается), `fsync`, затем переименование в цель ([`zam_core::fsx::rename_retry`]):
//!   оборванная загрузка цель не трогает, свой `.part` удаляется. После убитого процесса `.part`
//!   остаётся — его можно удалить вручную.
//! - Полнота архива — по записи конца центрального каталога zip ([`zip_complete`]): она стоит
//!   ровно в конце файла, а центральный каталог (и запись zip64) — ровно перед ней. Сервер
//!   обрывает тело при сбое посреди потока; конец вложенного zip (несжатый `.docx` в архиве) за
//!   конец архива не принимается.
//! - Сторож простоя: тело без единого байта дольше `ZAM_EXPORT_IDLE_SECS` (по умолчанию 300 с) —
//!   свой `.part` удаляется, выход с кодом 2. У ureq 3 нет тайм-аута простоя, а общий предел
//!   на тело архива любого размера ставить нельзя.
//! - Сервер старше маршрута отвечает 404 и на существующее пространство: тогда `zam` проверяет
//!   пространство (`GET /spaces/{space}/tree`) и, если оно есть, сообщает «сервер не
//!   поддерживает экспорт» (код 1) вместо «не найдено».
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::json;

use super::{Ctx, emit, emit_json};
use crate::client::{CliError, EXIT_CONFIG, EXIT_UNREACHABLE, Timeouts, into_reader};

/// Сообщение для сервера без маршрута экспорта.
pub const OLD_SERVER: &str =
    "сервер не поддерживает экспорт пространства: нужен zam-server 0.0.11-m6 или новее";

/// Переменная предела простоя тела, с.
pub const IDLE_ENV: &str = "ZAM_EXPORT_IDLE_SECS";
/// Предел простоя тела по умолчанию.
const IDLE_DEFAULT: Duration = Duration::from_secs(300);

/// Аргументы `zam export`.
pub struct ExportArgs<'a> {
    pub space: Option<&'a str>,
    pub paths: &'a [String],
    pub kinds: &'a [String],
    pub no_history: bool,
    pub out: &'a Path,
}

/// Пространство экспорта (см. модуль).
fn space_of(ctx: &Ctx, flag: Option<&str>) -> Result<String, CliError> {
    match flag.map(str::trim) {
        Some("solution") => ctx.solution.as_ref().map(|s| s.id.clone()).ok_or_else(|| {
            CliError::config(
                "--space solution требует .zeroaimemory/solution.toml в текущем каталоге или выше",
            )
        }),
        Some("") => Err(CliError::input("--space: пустое значение")),
        Some(s) => Ok(s.to_string()),
        None => Ok(ctx
            .solution
            .as_ref()
            .map_or_else(|| "knowledge".to_string(), |s| s.id.clone())),
    }
}

/// Список через запятую для параметра запроса; запятая внутри элемента — ошибка ввода.
fn joined(flag: &str, items: &[String]) -> Result<String, CliError> {
    let mut out = Vec::new();
    for it in items {
        let it = it.trim().trim_matches('/');
        if it.is_empty() {
            return Err(CliError::input(format!("{flag}: пустое значение")));
        }
        if it.contains(',') {
            return Err(CliError::input(format!(
                "{flag}: «,» в значении не поддерживается (разделитель списка)"
            )));
        }
        out.push(it.to_string());
    }
    Ok(out.join(","))
}

/// Временный файл загрузки рядом с целью: `<имя>.<pid>[-N].part`, создаётся заново
/// (`create_new`) — чужой файл с таким именем не перезаписывается и не удаляется.
fn create_part(out: &Path) -> std::io::Result<(PathBuf, File)> {
    let name = out
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "export.zip".into());
    let pid = std::process::id();
    let mut n = 0u32;
    loop {
        let file = match n {
            0 => format!("{name}.{pid}.part"),
            _ => format!("{name}.{pid}-{n}.part"),
        };
        let p = out.with_file_name(file);
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&p)
        {
            Ok(f) => return Ok((p, f)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && n < 100 => n += 1,
            Err(e) => return Err(e),
        }
    }
}

const EOCD: &[u8; 4] = b"PK\x05\x06";
const ZIP64_LOCATOR: &[u8; 4] = b"PK\x06\x07";
const ZIP64_EOCD: &[u8; 4] = b"PK\x06\x06";
const CD_HEADER: &[u8; 4] = b"PK\x01\x02";

fn u16_at(b: &[u8], i: usize) -> u64 {
    u64::from(u16::from_le_bytes([b[i], b[i + 1]]))
}

fn u32_at(b: &[u8], i: usize) -> u64 {
    u64::from(u32::from_le_bytes(b[i..i + 4].try_into().expect("4 байта")))
}

fn u64_at(b: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(b[i..i + 8].try_into().expect("8 байт"))
}

/// `n` байт файла с позиции `at` (короче — `None`).
fn read_at<F: Read + Seek>(f: &mut F, at: u64, n: usize) -> std::io::Result<Option<Vec<u8>>> {
    f.seek(SeekFrom::Start(at))?;
    let mut b = Vec::with_capacity(n);
    Read::by_ref(f).take(n as u64).read_to_end(&mut b)?;
    Ok((b.len() == n).then_some(b))
}

/// Центральный каталог `[cd_off, cd_off + cd_size)` кончается ровно на `end` и (если не пуст)
/// начинается с заголовка записи каталога.
fn cd_fits<F: Read + Seek>(
    f: &mut F,
    cd_off: u64,
    cd_size: u64,
    end: u64,
) -> std::io::Result<bool> {
    if cd_off.checked_add(cd_size) != Some(end) {
        return Ok(false);
    }
    if cd_size == 0 {
        return Ok(true);
    }
    Ok(read_at(f, cd_off, 4)?.is_some_and(|b| b == CD_HEADER))
}

/// Архив zip завершён (см. модуль): запись конца каталога заканчивается ровно в конце файла
/// (с комментарием), центральный каталог — ровно перед ней (или перед записью zip64, на которую
/// указывает локатор).
pub fn zip_complete<F: Read + Seek>(f: &mut F) -> std::io::Result<bool> {
    let len = f.seek(SeekFrom::End(0))?;
    if len < 22 {
        return Ok(false);
    }
    let tail = len.min(22 + 65_535);
    let base = len - tail;
    let b = read_at(f, base, tail as usize)?.unwrap_or_default();
    for p in (0..=b.len() - 22).rev() {
        if &b[p..p + 4] != EOCD || base + p as u64 + 22 + u16_at(&b, p + 20) != len {
            continue;
        }
        let eocd = base + p as u64;
        // zip64: локатор (20 байт) прямо перед записью конца
        if eocd >= 20
            && let Some(loc) = read_at(f, eocd - 20, 20)?
            && &loc[..4] == ZIP64_LOCATOR
        {
            let z64 = u64_at(&loc, 8);
            let Some(rec) = read_at(f, z64, 56)? else {
                return Ok(false);
            };
            return Ok(&rec[..4] == ZIP64_EOCD
                && z64.checked_add(56 + u64_at(&rec, 4).saturating_sub(44)) == Some(eocd - 20)
                && cd_fits(f, u64_at(&rec, 48), u64_at(&rec, 40), z64)?);
        }
        return cd_fits(f, u32_at(&b, p + 16), u32_at(&b, p + 12), eocd);
    }
    Ok(false)
}

/// Предел простоя тела: `ZAM_EXPORT_IDLE_SECS` (целое > 0), иначе [`IDLE_DEFAULT`].
fn idle_limit() -> Duration {
    std::env::var(IDLE_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&s| s > 0)
        .map_or(IDLE_DEFAULT, Duration::from_secs)
}

/// Чтение с отметкой времени последнего полученного байта.
struct Watched<R> {
    inner: R,
    t0: Instant,
    last_ms: Arc<AtomicU64>,
}

impl<R: Read> Read for Watched<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.last_ms
                .store(self.t0.elapsed().as_millis() as u64, Ordering::Relaxed);
        }
        Ok(n)
    }
}

/// Сторож простоя (см. модуль): поток, который удаляет `part` и завершает процесс с кодом 2,
/// если байты не приходят дольше `limit`. Снимается флагом `done`.
fn watchdog(
    part: PathBuf,
    limit: Duration,
    t0: Instant,
    last_ms: Arc<AtomicU64>,
) -> Arc<AtomicBool> {
    let done = Arc::new(AtomicBool::new(false));
    let d = done.clone();
    let tick = (limit / 4).clamp(Duration::from_millis(100), Duration::from_secs(1));
    std::thread::spawn(move || {
        while !d.load(Ordering::Relaxed) {
            std::thread::sleep(tick);
            let idle = t0
                .elapsed()
                .saturating_sub(Duration::from_millis(last_ms.load(Ordering::Relaxed)));
            if idle > limit && !d.load(Ordering::Relaxed) {
                let _ = std::fs::remove_file(&part);
                eprintln!(
                    "загрузка архива прервана: сервер молчит дольше {} с ({IDLE_ENV})",
                    limit.as_secs()
                );
                std::process::exit(i32::from(EXIT_UNREACHABLE));
            }
        }
    });
    done
}

/// Тело ответа → `part` (открытый файл `f`) под сторожем простоя; размер.
fn download(
    resp: ureq::http::Response<ureq::Body>,
    part: &Path,
    mut f: File,
) -> Result<u64, CliError> {
    let io_err = |e: std::io::Error| CliError::config(format!("{}: {e}", part.display()));
    let t0 = Instant::now();
    let last_ms = Arc::new(AtomicU64::new(0));
    let done = watchdog(part.to_path_buf(), idle_limit(), t0, last_ms.clone());
    let mut r = Watched {
        inner: into_reader(resp),
        t0,
        last_ms,
    };
    let copied = std::io::copy(&mut r, &mut f);
    done.store(true, Ordering::Relaxed);
    let n = copied
        .map_err(|e| CliError::new(EXIT_UNREACHABLE, format!("загрузка архива прервана: {e}")))?;
    f.flush().map_err(io_err)?;
    if !zip_complete(&mut f).map_err(io_err)? {
        return Err(CliError::new(
            EXIT_UNREACHABLE,
            "архив не завершён: сервер оборвал поток (см. журнал сервера)",
        ));
    }
    f.sync_all().map_err(io_err)?;
    Ok(n)
}

pub fn export(ctx: &mut Ctx, a: &ExportArgs) -> Result<(), CliError> {
    let space = space_of(ctx, a.space)?;
    let paths = joined("--path", a.paths)?;
    let kinds = joined("--kind", a.kinds)?;
    let mut q: Vec<(&str, &str)> = Vec::new();
    if !paths.is_empty() {
        q.push(("paths", &paths));
    }
    if !kinds.is_empty() {
        q.push(("kinds", &kinds));
    }
    if a.no_history {
        q.push(("history", "false"));
    }
    if a.out.as_os_str().is_empty() || a.out.is_dir() {
        return Err(CliError::input("-o: нужен путь файла .zip"));
    }
    let path = format!("/spaces/{space}/export");
    ctx.client.set_timeouts(Timeouts::Download);
    let resp = match ctx.client.get(&path, &q) {
        Ok(r) => r,
        Err(e) if e.http_status() == Some(404) => {
            ctx.client.set_timeouts(Timeouts::Transfer);
            // старый сервер: маршрута нет, а пространство есть
            if ctx
                .client
                .get(&format!("/spaces/{space}/tree"), &[])
                .is_ok()
            {
                return Err(CliError::new(EXIT_CONFIG, OLD_SERVER));
            }
            return Err(e);
        }
        Err(e) => return Err(e),
    };
    let (part, f) =
        create_part(a.out).map_err(|e| CliError::config(format!("{}: {e}", a.out.display())))?;
    let res = download(resp, &part, f).and_then(|n| {
        zam_core::fsx::rename_retry(&part, a.out, |_, _, _| {})
            .map_err(|e| CliError::config(format!("{}: {e}", a.out.display())))?;
        Ok(n)
    });
    let bytes = match res {
        Ok(n) => n,
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            return Err(e);
        }
    };
    if ctx.json {
        return emit_json(
            &json!({"file": a.out.display().to_string(), "bytes": bytes, "space": space}),
        );
    }
    emit(format!("{} ({bytes} байт)\n", a.out.display()).as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn joined_rules() {
        assert_eq!(
            joined("--path", &["docs/".into(), " a/b".into()]).unwrap(),
            "docs,a/b"
        );
        assert!(joined("--path", &["a,b".into()]).is_err());
        assert!(joined("--kind", &[" ".into()]).is_err());
        assert_eq!(joined("--kind", &[]).unwrap(), "");
    }

    #[test]
    fn part_is_new_file_next_to_target() {
        let d =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-tmp/zam-cli/part-unit");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let out = d.join("a.zip");
        let (p1, _f1) = create_part(&out).unwrap();
        let (p2, _f2) = create_part(&out).unwrap();
        assert_eq!(p1.parent(), Some(d.as_path()));
        assert_ne!(p1, p2);
        assert!(p1.file_name().unwrap().to_string_lossy().ends_with(".part"));
    }

    /// Минимальный zip: одна запись каталога длиной 46 байт с именем `a`, без данных.
    fn tiny_zip(prefix: &[u8], comment: &[u8]) -> Vec<u8> {
        let mut v = prefix.to_vec();
        let cd_off = v.len() as u32;
        let mut cd = CD_HEADER.to_vec();
        cd.extend_from_slice(&[0u8; 24]);
        cd.extend_from_slice(&1u16.to_le_bytes()); // длина имени
        cd.extend_from_slice(&[0u8; 16]);
        cd.push(b'a');
        v.extend_from_slice(&cd);
        v.extend_from_slice(EOCD);
        v.extend_from_slice(&[0, 0, 0, 0, 1, 0, 1, 0]);
        v.extend_from_slice(&(cd.len() as u32).to_le_bytes());
        v.extend_from_slice(&cd_off.to_le_bytes());
        v.extend_from_slice(&(comment.len() as u16).to_le_bytes());
        v.extend_from_slice(comment);
        v
    }

    #[test]
    fn zip_end_exact() {
        let ok = tiny_zip(b"local-data", b"");
        assert!(zip_complete(&mut Cursor::new(ok.clone())).unwrap());
        assert!(zip_complete(&mut Cursor::new(tiny_zip(b"x", b"comment"))).unwrap());
        // вложенный zip целиком в конце обрезанного потока: его конец — не конец архива
        let mut cut = b"PK\x03\x04header-of-outer-archive...".to_vec();
        cut.extend_from_slice(&ok);
        assert!(!zip_complete(&mut Cursor::new(cut)).unwrap());
        // лишние байты после конца каталога
        let mut tail = ok.clone();
        tail.push(0);
        assert!(!zip_complete(&mut Cursor::new(tail)).unwrap());
        // пустой zip (только запись конца) — завершён
        let mut empty = EOCD.to_vec();
        empty.extend_from_slice(&[0u8; 18]);
        assert!(zip_complete(&mut Cursor::new(empty)).unwrap());
        assert!(!zip_complete(&mut Cursor::new(b"PK\x03\x04".to_vec())).unwrap());
    }

    #[test]
    fn zip64_end_exact() {
        // каталог, затем запись zip64 (56 байт), локатор (20), классическая запись конца с 0xFFFF…
        let mut v = b"data".to_vec();
        let cd_off = v.len() as u64;
        let mut cd = CD_HEADER.to_vec();
        cd.extend_from_slice(&[0u8; 42]);
        v.extend_from_slice(&cd);
        let z64 = v.len() as u64;
        v.extend_from_slice(ZIP64_EOCD);
        v.extend_from_slice(&44u64.to_le_bytes());
        v.extend_from_slice(&[0u8; 28]);
        v.extend_from_slice(&(cd.len() as u64).to_le_bytes());
        v.extend_from_slice(&cd_off.to_le_bytes());
        v.extend_from_slice(ZIP64_LOCATOR);
        v.extend_from_slice(&0u32.to_le_bytes());
        v.extend_from_slice(&z64.to_le_bytes());
        v.extend_from_slice(&1u32.to_le_bytes());
        v.extend_from_slice(EOCD);
        v.extend_from_slice(&[0xFF; 16]);
        v.extend_from_slice(&0u16.to_le_bytes());
        assert!(zip_complete(&mut Cursor::new(v.clone())).unwrap());
        // локатор указывает не туда
        let bad_at = v.len() - 22 - 20 + 8;
        v[bad_at] ^= 1;
        assert!(!zip_complete(&mut Cursor::new(v)).unwrap());
    }
}
