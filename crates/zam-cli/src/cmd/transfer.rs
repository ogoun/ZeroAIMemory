//! Перенос пространства (M6 §12б.5): `zam export [--space S] [--path P]… [--kind K]…
//! [--no-history] -o <файл.zip>` — архив `GET /spaces/{space}/export` в файл; `zam import
//! <файл.zip|каталог> [--space S] [--own] [--on-conflict skip|revision] [--axioms demote|keep]
//! [--dry-run]` — `POST /spaces/{space}/import` потоком (каталог упаковывается в zip на лету,
//! без временного файла; каталог без `manifest.json` — «папка `.md`»: на сервер уходят только
//! `*.md`, архив чужой). Сервер отвечает 202 с заданием; `zam` опрашивает его раз в 2 с, печатает
//! этап в stderr и выходит с кодом 2, если хода нет дольше `ZAM_IMPORT_IDLE_SECS` (600 с). Ctrl+C
//! прекращает только ожидание: задание на сервере продолжается.
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

/// Сообщение для сервера без маршрута импорта.
pub const OLD_SERVER_IMPORT: &str =
    "сервер не поддерживает импорт пространства: нужен zam-server 0.0.11-m6 или новее";

/// Аргументы `zam import`.
pub struct ImportArgs<'a> {
    pub src: &'a Path,
    pub space: Option<&'a str>,
    pub own: bool,
    pub on_conflict: Option<&'a str>,
    pub axioms: Option<&'a str>,
    pub dry_run: bool,
}

/// Переменная предела отсутствия хода задания импорта, с.
pub const IMPORT_IDLE_ENV: &str = "ZAM_IMPORT_IDLE_SECS";
/// Предел отсутствия хода задания по умолчанию.
const IMPORT_IDLE_DEFAULT: Duration = Duration::from_secs(600);
/// Шаг опроса задания.
const POLL: Duration = Duration::from_secs(2);

/// Файлы архива (путь в архиве, файл на диске) и пропущенные скрытые пути.
type DirFiles = (Vec<(String, PathBuf)>, Vec<String>);

/// Файлы каталога `root` для архива: относительные пути через `/`, по алфавиту; скрытые
/// (`.git`, `.zeroaimemory`) пропускаются и перечисляются вторым списком. Без описи — только
/// `*.md`.
fn dir_files(root: &Path) -> std::io::Result<DirFiles> {
    let manifest = root.join("manifest.json").is_file();
    let mut out = Vec::new();
    let mut hidden = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    let rel = |p: &Path| -> std::io::Result<String> {
        Ok(p.strip_prefix(root)
            .map_err(std::io::Error::other)?
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"))
    };
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d)? {
            let e = e?;
            let name = e.file_name().to_string_lossy().into_owned();
            let p = e.path();
            if name.starts_with('.') {
                hidden.push(rel(&p)?);
                continue;
            }
            let t = e.file_type()?;
            if t.is_dir() {
                stack.push(p);
            } else if t.is_file() && (manifest || name.ends_with(".md")) {
                out.push((rel(&p)?, p));
            }
        }
    }
    out.sort();
    hidden.sort();
    Ok((out, hidden))
}

/// Архив каталога потоком (поток-упаковщик пишет zip в канал, тело запроса читает его):
/// временного файла нет — прерванный `zam` ничего не оставляет. Ошибка упаковки — итог потока.
fn pack_stream(
    files: Vec<(String, PathBuf)>,
) -> std::io::Result<(
    std::io::PipeReader,
    std::thread::JoinHandle<Result<(), String>>,
)> {
    let (r, w) = std::io::pipe()?;
    let h = std::thread::spawn(move || -> Result<(), String> {
        let mut z = zip::ZipWriter::new_stream(w);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .large_file(true);
        for (rel, p) in &files {
            z.start_file(rel.as_str(), opts)
                .map_err(|e| format!("zip: {e}"))?;
            let mut src = File::open(p).map_err(|e| format!("{}: {e}", p.display()))?;
            std::io::copy(&mut src, &mut z).map_err(|e| format!("{}: {e}", p.display()))?;
        }
        z.finish().map_err(|e| format!("zip: {e}"))?;
        Ok(())
    });
    Ok((r, h))
}

/// Предел отсутствия хода: `ZAM_IMPORT_IDLE_SECS` (целое > 0), иначе [`IMPORT_IDLE_DEFAULT`].
fn import_idle() -> Duration {
    std::env::var(IMPORT_IDLE_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&s| s > 0)
        .map_or(IMPORT_IDLE_DEFAULT, Duration::from_secs)
}

/// Код выхода по статусу ошибки задания.
fn exit_of(status: u64) -> u8 {
    match status {
        404 => crate::client::EXIT_NOT_FOUND,
        409 => crate::client::EXIT_CONFLICT,
        401 | 403 => crate::client::EXIT_TOKEN,
        400..=499 => crate::client::EXIT_INPUT,
        _ => EXIT_UNREACHABLE,
    }
}

/// Опрос задания раз в [`POLL`]; этап печатается в stderr при смене; хода нет дольше
/// [`import_idle`] — выход с кодом 2 (задание на сервере продолжается).
fn poll_job(ctx: &Ctx, status: &str, job: &str) -> Result<serde_json::Value, CliError> {
    let idle = import_idle();
    let mut last = (String::new(), u64::MAX);
    let mut moved = Instant::now();
    loop {
        let s = crate::client::read_json(ctx.client.get(status, &[])?)?;
        let stage = s["stage"].as_str().unwrap_or("?").to_string();
        let done = s["progress"]["done"].as_u64().unwrap_or(0);
        if (stage.clone(), done) != last {
            if stage != last.0 {
                let total = s["progress"]["total"].as_u64().unwrap_or(0);
                eprintln!("импорт {job}: {stage} ({done}/{total})");
            }
            last = (stage, done);
            moved = Instant::now();
        }
        match s["state"].as_str() {
            Some("done") => return Ok(s["report"].clone()),
            Some("failed") => {
                let e = &s["error"];
                return Err(CliError::new(
                    exit_of(e["status"].as_u64().unwrap_or(500)),
                    format!(
                        "импорт не выполнен: {}",
                        e["detail"]
                            .as_str()
                            .or(e["title"].as_str())
                            .unwrap_or("ошибка сервера")
                    ),
                ));
            }
            _ => {}
        }
        if moved.elapsed() > idle {
            return Err(CliError::new(
                EXIT_UNREACHABLE,
                format!(
                    "сервер не сообщает о ходе импорта дольше {} с ({IMPORT_IDLE_ENV}); задание \
                     {job} продолжается на сервере: GET /api/v1{status}",
                    idle.as_secs()
                ),
            ));
        }
        std::thread::sleep(POLL);
    }
}

pub fn import(ctx: &mut Ctx, a: &ImportArgs) -> Result<(), CliError> {
    let space = space_of(ctx, a.space)?;
    let mut q: Vec<String> = Vec::new();
    if a.own {
        q.push("own=true".into());
    }
    if let Some(c) = a.on_conflict {
        q.push(format!("on_conflict={c}"));
    }
    if let Some(x) = a.axioms {
        q.push(format!("axioms={x}"));
    }
    if a.dry_run {
        q.push("dry_run=true".into());
    }
    let path = if q.is_empty() {
        format!("/spaces/{space}/import")
    } else {
        format!("/spaces/{space}/import?{}", q.join("&"))
    };
    // приём тела может идти долго: тело без общего предела, заголовки — как у экспорта
    ctx.client.set_timeouts(Timeouts::Download);
    let (sent, packer) = if a.src.is_dir() {
        let io = |e: std::io::Error| CliError::config(format!("{}: {e}", a.src.display()));
        let (files, hidden) = dir_files(a.src).map_err(io)?;
        if !hidden.is_empty() {
            eprintln!(
                "предупреждение: скрытые пути не переносятся ({}): {}",
                hidden.len(),
                hidden.join(", ")
            );
        }
        if files.is_empty() {
            return Err(CliError::input(format!(
                "{}: нет файлов .md для импорта",
                a.src.display()
            )));
        }
        let (r, h) = pack_stream(files).map_err(io)?;
        (ctx.client.send_stream(&path, "application/zip", r), Some(h))
    } else if a.src.is_file() {
        let f =
            File::open(a.src).map_err(|e| CliError::config(format!("{}: {e}", a.src.display())))?;
        let len = f
            .metadata()
            .map_err(|e| CliError::config(format!("{}: {e}", a.src.display())))?
            .len();
        (
            ctx.client.send_multipart(&path, "application/zip", len, f),
            None,
        )
    } else {
        return Err(CliError::input(format!(
            "{}: нет такого файла или каталога",
            a.src.display()
        )));
    };
    // ошибка упаковки важнее ответа сервера на оборванный архив
    if let Some(h) = packer {
        match h.join() {
            Ok(Ok(())) => {}
            Ok(Err(m)) => return Err(CliError::config(format!("упаковка каталога: {m}"))),
            Err(_) => return Err(CliError::config("упаковка каталога прервана")),
        }
    }
    let resp = match sent {
        Ok(r) => r,
        Err(e) if e.http_status() == Some(404) => {
            ctx.client.set_timeouts(Timeouts::Transfer);
            if ctx
                .client
                .get(&format!("/spaces/{space}/tree"), &[])
                .is_ok()
            {
                return Err(CliError::new(EXIT_CONFIG, OLD_SERVER_IMPORT));
            }
            return Err(e);
        }
        Err(e) => return Err(e),
    };
    let accepted = crate::client::read_json(resp)?;
    let job = accepted["job"].as_str().unwrap_or_default().to_string();
    let status = accepted["status"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| CliError::new(EXIT_UNREACHABLE, "сервер не вернул задание импорта"))?;
    eprintln!(
        "импорт {job} принят сервером; Ctrl+C прекращает только ожидание — задание на сервере \
         не отменяется (ход: GET /api/v1{status})"
    );
    ctx.client.set_timeouts(Timeouts::Transfer);
    let rep = poll_job(ctx, &status, &job)?;
    if ctx.json {
        return emit_json(&rep);
    }
    emit(report_text(&space, &rep).as_bytes())
}

/// Отчёт импорта текстом.
fn report_text(space: &str, rep: &serde_json::Value) -> String {
    let n = |k: &str| rep[k].as_u64().unwrap_or(0);
    let mut s = format!(
        "{}импорт в {space}: создано {}, обновлено {}, пропущено {}, конфликтов {}, \
         аксиом понижено {}, id переназначено {}, связей пропущено {}\n",
        if rep["dry_run"] == json!(true) {
            "пробный "
        } else {
            ""
        },
        n("created"),
        n("revised"),
        n("skipped"),
        rep["conflicts"].as_array().map_or(0, Vec::len),
        n("demoted_axioms"),
        n("id_remapped"),
        n("links_skipped"),
    );
    for c in rep["conflicts"].as_array().into_iter().flatten() {
        let at = c["path"].as_str().or(c["id"].as_str()).unwrap_or("?");
        s.push_str(&format!(
            "конфликт: {at} — {}\n",
            c["reason"].as_str().unwrap_or("?")
        ));
    }
    for w in rep["warnings"].as_array().into_iter().flatten() {
        s.push_str(&format!(
            "предупреждение: {}\n",
            w.as_str().unwrap_or_default()
        ));
    }
    if let Some(u) = rep["unresolved"].as_array().filter(|u| !u.is_empty()) {
        s.push_str(&format!("ссылок вне архива: {}\n", u.len()));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn dir_files_md_only_without_manifest() {
        let d = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-tmp/zam-cli/import-dir-unit");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("sub")).unwrap();
        std::fs::create_dir_all(d.join(".git")).unwrap();
        std::fs::write(d.join("a.md"), "a").unwrap();
        std::fs::write(d.join("sub").join("b.md"), "b").unwrap();
        std::fs::write(d.join("pic.png"), "p").unwrap();
        std::fs::write(d.join(".git").join("c.md"), "c").unwrap();
        let (files, hidden) = dir_files(&d).unwrap();
        let names: Vec<String> = files.into_iter().map(|x| x.0).collect();
        assert_eq!(names, ["a.md", "sub/b.md"]);
        assert_eq!(hidden, [".git"]);
        std::fs::write(d.join("manifest.json"), "{}").unwrap();
        let names: Vec<String> = dir_files(&d).unwrap().0.into_iter().map(|x| x.0).collect();
        assert_eq!(names, ["a.md", "manifest.json", "pic.png", "sub/b.md"]);
        let _ = std::fs::remove_dir_all(&d);
    }

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
