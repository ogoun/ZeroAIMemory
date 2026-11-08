//! Перенос пространства (M6 §12б.5): `zam export [--space S] [--path P]… [--kind K]…
//! [--no-history] -o <файл.zip>` — архив `GET /spaces/{space}/export` в файл. Импорт — 18b2.
//!
//! - Пространство: `--space` (`rules` | `human` | `knowledge` | `solution` | GUID | id), иначе
//!   решение из `solution.toml`, иначе `knowledge` — как у адресов заметок.
//! - Архив пишется потоком в новый `<файл>.<pid>.part` рядом с целью, `fsync`, затем переименование в цель
//!   ([`zam_core::fsx::rename_retry`]): оборванная загрузка цель не трогает, `.part` удаляется.
//!   Конец архива проверяется по записи конца каталога zip (`PK\x05\x06`): сервер обрывает тело
//!   при сбое посреди потока.
//! - Сервер старше маршрута отвечает 404 и на существующее пространство: тогда `zam` проверяет
//!   пространство (`GET /spaces/{space}/tree`) и, если оно есть, сообщает «сервер не
//!   поддерживает экспорт» (код 1) вместо «не найдено».
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde_json::json;

use super::{Ctx, emit, emit_json};
use crate::client::{CliError, EXIT_CONFIG, EXIT_UNREACHABLE, Timeouts, into_reader};

/// Сообщение для сервера без маршрута экспорта.
pub const OLD_SERVER: &str =
    "сервер не поддерживает экспорт пространства: нужен zam-server 0.0.11-m6 или новее";

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

/// В последних байтах файла есть запись конца каталога zip.
fn has_zip_end(f: &mut File) -> std::io::Result<bool> {
    // запись конца каталога — 22 байта плюс комментарий до 64 КиБ
    let len = f.seek(SeekFrom::End(0))?;
    let tail = len.min(22 + 65_535);
    f.seek(SeekFrom::Start(len - tail))?;
    let mut b = Vec::with_capacity(tail as usize);
    Read::by_ref(f).take(tail).read_to_end(&mut b)?;
    Ok(b.windows(4).any(|w| w == b"PK\x05\x06"))
}

/// Тело ответа → `part` (открытый файл `f`); размер.
fn download(
    resp: ureq::http::Response<ureq::Body>,
    part: &Path,
    mut f: File,
) -> Result<u64, CliError> {
    let io_err = |e: std::io::Error| CliError::config(format!("{}: {e}", part.display()));
    let n = std::io::copy(&mut into_reader(resp), &mut f)
        .map_err(|e| CliError::new(EXIT_UNREACHABLE, format!("загрузка архива прервана: {e}")))?;
    f.flush().map_err(io_err)?;
    if !has_zip_end(&mut f).map_err(io_err)? {
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
}
