//! `zam report <файл.md> [--title T] [--tag X]… [--area A] [--ref commit:<sha>|file:<путь>|
//! note:<id>|task:k:<id>]… [--key K] [--solution G]` — отчёт для пользователя (M5.5 §6).
//!
//! - **Тело** — файл как есть (UTF-8). Заголовок — `--title`, иначе первый заголовок `#` файла
//!   (вне кода и frontmatter); нет — ошибка ввода (код 6).
//! - **Файлы отчёта** — локальные ссылки тела `![…](путь)`, `[…](путь)`, `(<путь с пробелом>)`,
//!   ссылки-сноски `[…][метка]` с определением `[метка]: путь`; вне кода, формул и frontmatter.
//!   Путь — относительно каталога файла, percent-декодируется, ведущие `./` снимаются: это и имя
//!   части `file` (сервер сверяет его с телом тем же правилом). Повтор пути — одна часть.
//!   Схемы из [`zam_core::REPORT_LINK_SCHEMES`] (`http:`, `https:`, `mailto:`, `vscode:`,
//!   `file:` — M6 §16 п. 15), якоря и `//…` — не файлы ([`resolve_local`] → `None`; картинку
//!   со схемой отклонит сервер). Прочая схема (`ftp:`, `javascript:`, `data:` …, в том числе
//!   с регистром, пробелом и табуляцией — [`zam_core::link::url_scheme`]), `..`, абсолютный
//!   путь, `\`, символы `" : * ? < > |`, выход за каталог (символическая ссылка), нет файла —
//!   ошибка ввода **до** любого запроса к серверу.
//! - **Пределы до отправки** — не больше [`MAX_FILES`] файлов и [`MAX_TOTAL_BYTES`] в сумме по
//!   `stat` (умолчания сервера `reports.*`; окончательно проверяет сервер).
//! - **Разбор ссылок** — свой лёгкий сканер ([`local_links`]): `zam` не тянет `comrak` в путь
//!   хуков (решение составителя плана 14); согласие с сервером держит тест на корпусе против
//!   `zam_markdown::parse` (`local_links_agrees_with_zam_markdown`).
//! - **Запрос** — `POST /spaces/{guid}/entries` multipart потоком ([`report_multipart`]: часть
//!   `meta`, затем части `file`; длина известна заранее, файлы в память не читаются). Без
//!   `--key` повтор команды — новый отчёт (отчёт неизменяем).
//! - **Вывод** — `Отчёт: <url>` (страница отчёта в WebUI для пользователя), `--json` — ответ
//!   сервера `{id, url}`. Запись отмечается в журнале сессии агента (`note_write("report")`).
use std::collections::{HashMap, VecDeque};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::{Ctx, emit, emit_json, solution_guid};
use crate::client::{CliError, read_json};
use crate::target::Loc;

/// Файлов в отчёте не больше (умолчание `reports.max_files` сервера).
pub const MAX_FILES: usize = 64;
/// Сумма файлов отчёта не больше, байт (умолчание `reports.max_total_bytes`, 200 МиБ).
pub const MAX_TOTAL_BYTES: u64 = 200 * 1024 * 1024;
/// Символы, запрещённые в пути файла (как у сервера; `\` и управляющие — отдельно).
const PATH_FORBIDDEN: [char; 7] = ['"', ':', '*', '?', '<', '>', '|'];
/// Пределы пути узла сервера (`zam_store::paths`): путь и сегмент (имя файла), байт.
const PATH_MAX_BYTES: usize = 1024;
const SEGMENT_MAX_BYTES: usize = 255;

/// Аргументы `zam report`.
pub struct ReportArgs<'a> {
    pub file: &'a Path,
    pub title: Option<&'a str>,
    pub tags: &'a [String],
    pub area: Option<&'a str>,
    pub refs: &'a [String],
    pub key: Option<&'a str>,
    pub solution: Option<&'a str>,
}

pub fn report(ctx: &Ctx, a: &ReportArgs) -> Result<(), CliError> {
    let bytes = std::fs::read(a.file)
        .map_err(|e| CliError::input(format!("чтение {}: {e}", a.file.display())))?;
    let md = String::from_utf8(bytes).map_err(|e| {
        CliError::input(format!(
            "{}: не UTF-8 (байт {})",
            a.file.display(),
            e.utf8_error().valid_up_to()
        ))
    })?;
    let title = match a.title {
        Some(t) if !t.trim().is_empty() => t.trim().to_string(),
        Some(_) => return Err(CliError::input("--title: пустой заголовок")),
        None => first_h1(&md).ok_or_else(|| {
            CliError::input(format!(
                "{}: нет заголовка — укажите --title или начните файл строкой «# Заголовок»",
                a.file.display()
            ))
        })?,
    };
    let base = match a.file.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let files = collect_files(&base, &md)?;
    let data = refs_data(a.refs)?;
    let mut meta = json!({ "title": title, "body": md, "tags": a.tags });
    if let Some(area) = a.area {
        meta["area"] = json!(area);
    }
    if let Some(refs) = data {
        meta["data"] = json!({ "refs": refs });
    }
    let guid = solution_guid(ctx, a.solution)?;
    let own = ctx.solution.as_ref().is_some_and(|s| s.id == guid);
    ctx.ensure_solution(&Loc::Path {
        space: guid.clone(),
        path: String::new(),
        own_solution: own,
    })?;
    // граница — случайный id: в содержимом файлов не встречается на практике
    let boundary = format!("zam-{}", zam_core::Id::new());
    let (content_type, len, body) = report_multipart(&boundary, &meta, &files);
    let headers: Vec<(&str, &str)> = a.key.map(|k| ("Idempotency-Key", k)).into_iter().collect();
    let r = ctx.client.send_multipart_with(
        &format!("/spaces/{guid}/entries"),
        &content_type,
        len,
        body,
        &headers,
    )?;
    let created = r.status().as_u16() == 201;
    let v = read_json(r)?;
    if created {
        ctx.note_write("report");
    }
    if ctx.json {
        return emit_json(&v);
    }
    emit(format!("Отчёт: {}\n", v["url"].as_str().unwrap_or("?")).as_bytes())
}

/// Файлы отчёта по ссылкам тела: (имя части, путь, размер) без повторов имени; пределы
/// [`MAX_FILES`] и [`MAX_TOTAL_BYTES`] — по `stat`.
fn collect_files(base: &Path, md: &str) -> Result<Vec<(String, PathBuf, u64)>, CliError> {
    let mut out: Vec<(String, PathBuf, u64)> = Vec::new();
    let mut total = 0u64;
    for link in local_links(md) {
        let Some(name) = local_name(&link)? else {
            continue;
        };
        if out.iter().any(|(n, _, _)| *n == name) {
            continue;
        }
        let path = resolve_local(base, &link)?.expect("локальное имя — локальный путь");
        let len = std::fs::metadata(&path)
            .map_err(|e| CliError::input(format!("файл «{name}»: {e}")))?
            .len();
        if out.len() >= MAX_FILES {
            return Err(CliError::input(format!(
                "файлов в отчёте больше {MAX_FILES} (reports.max_files): разделите отчёт"
            )));
        }
        total += len;
        if total > MAX_TOTAL_BYTES {
            return Err(CliError::input(format!(
                "файлы отчёта больше {} МиБ (reports.max_total_bytes)",
                MAX_TOTAL_BYTES / (1024 * 1024)
            )));
        }
        out.push((name, path, len));
    }
    Ok(out)
}

/// `--ref` → `data.refs`: `commit:`, `file:` (или путь — путь от корня проекта, как у
/// `zam log changelog`), `note:`, `task:` (`task:k:<id>`); пусто — `None`.
fn refs_data(refs: &[String]) -> Result<Option<Value>, CliError> {
    if refs.is_empty() {
        return Ok(None);
    }
    let bad = |raw: &str| {
        CliError::input(format!(
            "--ref «{raw}»: формат commit:<sha>, file:<путь> (или путь), note:<id> или task:k:<id>"
        ))
    };
    let (cwd, root) = super::refs::cli_root();
    let mut groups: Map<String, Value> = Map::new();
    for raw in refs {
        let (key, value) = match raw.split_once(':') {
            Some(("commit", v)) => ("commits", v.to_string()),
            Some(("note", v)) => ("notes", v.to_string()),
            Some(("task", v)) => ("tasks", v.to_string()),
            Some(("file", v)) => ("files", v.to_string()),
            // буква диска или `:` в имени файла (`a/b:c`) — путь
            Some((k, _)) if k.len() == 1 && k.bytes().all(|b| b.is_ascii_alphabetic()) => {
                ("files", raw.clone())
            }
            Some((k, _)) if k.contains(['/', '\\']) => ("files", raw.clone()),
            Some(_) => return Err(bad(raw)),
            None => ("files", raw.clone()),
        };
        if value.trim().is_empty() {
            return Err(bad(raw));
        }
        let value = if key == "files" {
            super::refs::ref_path(&value, &cwd, &root)
                .map_err(|e| CliError::input(format!("--ref «{raw}»: {e}")))?
        } else {
            value.trim().to_string()
        };
        let list = groups
            .entry(key)
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .expect("массив");
        if !list.iter().any(|x| x.as_str() == Some(value.as_str())) {
            list.push(json!(value));
        }
    }
    Ok(Some(Value::Object(groups)))
}

// ---- multipart ----

/// Тело multipart отчёта: часть `meta` (JSON), затем по части `file` на файл (имя — путь из
/// тела; `filename` в UTF-8 и `filename*` RFC 8187 с тем же именем). Возвращает `Content-Type`,
/// длину и цепочку чтения (файлы открываются при чтении, в память не читаются).
pub fn report_multipart(
    boundary: &str,
    meta: &Value,
    files: &[(String, PathBuf, u64)],
) -> (String, u64, Box<dyn Read>) {
    let mut parts: VecDeque<Box<dyn Read>> = VecDeque::new();
    let mut total = 0u64;
    let mut push = |parts: &mut VecDeque<Box<dyn Read>>, b: Vec<u8>| {
        total += b.len() as u64;
        parts.push_back(Box::new(Cursor::new(b)));
    };
    push(
        &mut parts,
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"meta\"\r\n\
             Content-Type: application/json\r\n\r\n{meta}\r\n"
        )
        .into_bytes(),
    );
    for (name, path, len) in files {
        push(
            &mut parts,
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; \
                 filename=\"{name}\"; filename*=UTF-8''{}\r\n\
                 Content-Type: application/octet-stream\r\n\r\n",
                ext_value(name)
            )
            .into_bytes(),
        );
        parts.push_back(Box::new(LazyFile::new(path.clone(), *len)));
        push(&mut parts, b"\r\n".to_vec());
    }
    push(&mut parts, format!("--{boundary}--\r\n").into_bytes());
    let total = total + files.iter().map(|(_, _, l)| *l).sum::<u64>();
    (
        format!("multipart/form-data; boundary={boundary}"),
        total,
        Box::new(Concat(parts)),
    )
}

/// Значение `filename*` (RFC 8187): `attr-char` как есть, остальное — `%XX` байтов UTF-8.
fn ext_value(name: &str) -> String {
    name.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// Последовательное чтение частей.
struct Concat(VecDeque<Box<dyn Read>>);

impl Read for Concat {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        while let Some(r) = self.0.front_mut() {
            let n = r.read(buf)?;
            if n > 0 || buf.is_empty() {
                return Ok(n);
            }
            self.0.pop_front();
        }
        Ok(0)
    }
}

/// Файл ровно `len` байт, открывается при первом чтении; файл стал короче или длиннее — ошибка
/// (иначе тело не совпало бы с `Content-Length` или ушёл бы обрезанный файл).
struct LazyFile {
    path: PathBuf,
    left: u64,
    file: Option<std::io::Take<std::fs::File>>,
}

impl LazyFile {
    fn new(path: PathBuf, len: u64) -> LazyFile {
        LazyFile {
            path,
            left: len,
            file: None,
        }
    }
}

impl Read for LazyFile {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.left == 0 {
            return Ok(0);
        }
        if self.file.is_none() {
            self.file = Some(std::fs::File::open(&self.path)?.take(self.left));
        }
        let n = self.file.as_mut().expect("открыт").read(buf)?;
        if n == 0 {
            return Err(std::io::Error::other(format!(
                "{}: файл стал короче во время отправки",
                self.path.display()
            )));
        }
        self.left -= n as u64;
        if self.left == 0 {
            // заявленная длина отдана: в файле больше быть не должно
            let mut probe = [0u8; 1];
            let rest = self
                .file
                .as_mut()
                .expect("открыт")
                .get_mut()
                .read(&mut probe)?;
            if rest > 0 {
                return Err(std::io::Error::other(format!(
                    "{}: файл вырос во время отправки",
                    self.path.display()
                )));
            }
        }
        Ok(n)
    }
}

// ---- пути ----

/// Имя файла отчёта по назначению ссылки: `None` — не файл (разрешённая схема, якорь, `//`);
/// путь percent-декодирован, без якоря и ведущих `./`; схема не из
/// [`zam_core::REPORT_LINK_SCHEMES`] и нарушение правил пути — ошибка ввода.
fn local_name(link: &str) -> Result<Option<String>, CliError> {
    let t = link.trim();
    if t.is_empty() || t.starts_with('#') || t.starts_with("//") {
        return Ok(None);
    }
    let bad = |why: &str| CliError::input(format!("ссылка «{t}»: {why}"));
    let b = t.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        return Err(bad(
            "абсолютный путь — только путь относительно файла отчёта",
        ));
    }
    if let Some(scheme) = zam_core::link::url_scheme(t) {
        return if zam_core::link::report_scheme_allowed(&scheme) {
            Ok(None)
        } else {
            Err(bad(&format!(
                "схема {scheme}: не поддерживается ({} или файл отчёта)",
                zam_core::link::report_schemes_text()
            )))
        };
    }
    let path = t.split('#').next().unwrap_or_default();
    if path.is_empty() {
        return Ok(None);
    }
    let decoded = percent_decode(path).ok_or_else(|| bad("неверная percent-последовательность"))?;
    if decoded.starts_with('/') {
        return Err(bad(
            "абсолютный путь — только путь относительно файла отчёта",
        ));
    }
    if decoded.contains('\\') {
        return Err(bad("«\\» в пути — разделитель только «/»"));
    }
    let mut name = decoded.as_str();
    while let Some(rest) = name.strip_prefix("./") {
        name = rest;
    }
    if let Some(c) = name
        .chars()
        .find(|c| PATH_FORBIDDEN.contains(c) || c.is_control())
    {
        return Err(bad(&format!("символ {c:?} в пути не допускается")));
    }
    if name.ends_with('/') {
        return Err(bad(
            "ссылка на каталог — в отчёт прикладываются только файлы",
        ));
    }
    // правила пути узла и имени файла сервера (`NodePath::parse`, `check_filename`)
    if name.len() > PATH_MAX_BYTES {
        return Err(bad(&format!("путь длиннее {PATH_MAX_BYTES} байт")));
    }
    for seg in name.split('/') {
        match seg {
            ".." => return Err(bad("«..» в пути — файл отчёта только внутри его каталога")),
            "" | "." => return Err(bad("пустой сегмент или «.» в пути")),
            s if s.len() > SEGMENT_MAX_BYTES => {
                return Err(bad(&format!(
                    "имя файла или каталога длиннее {SEGMENT_MAX_BYTES} байт"
                )));
            }
            _ => {}
        }
    }
    if name.split('/').next() == Some("_attachments") {
        return Err(bad(
            "каталог _attachments/ занят вложениями — переименуйте его",
        ));
    }
    Ok(Some(name.to_string()))
}

/// Локальный файл по назначению ссылки относительно `base`: `None` — не локальный (схема,
/// якорь, `//`); ошибка — `..`, абсолютный путь, `\`, выход за `base` (по `canonicalize`,
/// символическая ссылка наружу), нет файла или не обычный файл.
pub(crate) fn resolve_local(base: &Path, link: &str) -> Result<Option<PathBuf>, CliError> {
    let Some(name) = local_name(link)? else {
        return Ok(None);
    };
    let mut path = base.to_path_buf();
    for seg in name.split('/') {
        path.push(seg);
    }
    let real = path.canonicalize().map_err(|_| {
        CliError::input(format!(
            "файл «{name}» не найден (путь — относительно {})",
            base.display()
        ))
    })?;
    let root = base
        .canonicalize()
        .map_err(|e| CliError::input(format!("каталог отчёта {}: {e}", base.display())))?;
    if !real.starts_with(&root) {
        return Err(CliError::input(format!(
            "файл «{name}» вне каталога отчёта (символическая ссылка наружу)"
        )));
    }
    if !real.is_file() {
        return Err(CliError::input(format!("«{name}» — не обычный файл")));
    }
    Ok(Some(real))
}

/// `%XX` → байты; `None` — неверная последовательность или не UTF-8.
fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            if !hex.bytes().all(|h| h.is_ascii_hexdigit()) {
                return None;
            }
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

// ---- сканер Markdown ----

/// Вид строки для правил блоков CommonMark.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Line {
    /// Пустая строка, начало документа, граница блока (заголовок, код, определение).
    Break,
    /// Строка абзаца (за ней отступ — продолжение, а не код; определение абзац не прерывает).
    Para,
}

/// Блоки документа: текст для разбора строк (код, frontmatter и определения ссылок заменены
/// пробелами — смещения те же), определения `[метка]: путь` (первое по тексту) и первый
/// заголовок `#`.
struct Blocks {
    text: Vec<u8>,
    defs: HashMap<String, String>,
    h1: Option<String>,
}

fn blocks(md: &str) -> Blocks {
    let mut text = md.as_bytes().to_vec();
    let mut defs = HashMap::new();
    let mut h1 = None;
    fn blank(text: &mut [u8], r: std::ops::Range<usize>) {
        for c in &mut text[r] {
            if *c != b'\n' && *c != b'\r' {
                *c = b' ';
            }
        }
    }
    let mut lines: Vec<std::ops::Range<usize>> = Vec::new();
    let mut s = 0;
    for (i, c) in md.bytes().enumerate() {
        if c == b'\n' {
            lines.push(s..i);
            s = i + 1;
        }
    }
    lines.push(s..md.len());
    let line_str = |r: &std::ops::Range<usize>| md[r.clone()].trim_end_matches('\r');
    let mut k = 0;
    // frontmatter `---` … `---` в начале документа (разбор сервера его не видит)
    if lines
        .first()
        .is_some_and(|r| line_str(r).trim_start_matches('\u{feff}') == "---")
        && let Some(end) = lines.iter().skip(1).position(|r| line_str(r) == "---")
    {
        for r in &lines[..end + 2] {
            blank(&mut text, r.clone());
        }
        k = end + 2;
    }
    let mut prev = Line::Break;
    let mut fence: Option<(u8, usize)> = None;
    let mut in_list = false;
    while k < lines.len() {
        let r = lines[k].clone();
        let l = line_str(&r);
        k += 1;
        let indent = indent_of(l);
        let rest = l.trim_start_matches([' ', '\t']);
        if let Some((ch, n)) = fence {
            let run = rest.bytes().take_while(|&c| c == ch).count();
            if indent < 4 && run >= n && rest[run..].trim().is_empty() {
                fence = None;
            }
            blank(&mut text, r);
            prev = Line::Break;
            continue;
        }
        if rest.is_empty() {
            prev = Line::Break;
            continue;
        }
        if indent >= 4 && prev == Line::Break && !in_list {
            // отступной блок кода
            blank(&mut text, r);
            continue;
        }
        if indent < 4 {
            let ch = rest.as_bytes()[0];
            let run = rest.bytes().take_while(|&c| c == ch).count();
            if (ch == b'`' || ch == b'~') && run >= 3 && !(ch == b'`' && rest[run..].contains('`'))
            {
                fence = Some((ch, run));
                blank(&mut text, r);
                prev = Line::Break;
                continue;
            }
            if let Some(h) = atx_heading(rest) {
                if h.0 == 1 && h1.is_none() && !h.1.is_empty() {
                    h1 = Some(h.1.to_string());
                }
                prev = Line::Break;
                continue;
            }
            // определение сноски `[^1]: …` — блок текста: его ссылки ищутся как обычно
            if prev == Line::Break
                && let Some((label, dest)) = definition(rest)
                && !label.starts_with('^')
            {
                defs.entry(norm_label(&label)).or_insert(dest);
                blank(&mut text, r);
                prev = Line::Break;
                continue;
            }
        }
        if list_marker(rest) && indent < 4 {
            in_list = true;
        } else if indent == 0 && prev == Line::Break {
            in_list = false;
        }
        prev = Line::Para;
    }
    Blocks { text, defs, h1 }
}

/// Отступ строки в колонках (табуляция — до кратного 4).
fn indent_of(l: &str) -> usize {
    let mut n = 0;
    for c in l.bytes() {
        match c {
            b' ' => n += 1,
            b'\t' => n += 4 - n % 4,
            _ => break,
        }
    }
    n
}

/// Заголовок ATX `#…######`: уровень и текст без закрывающих `#`.
fn atx_heading(rest: &str) -> Option<(usize, &str)> {
    let level = rest.bytes().take_while(|&c| c == b'#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let after = &rest[level..];
    if !(after.is_empty() || after.starts_with([' ', '\t'])) {
        return None;
    }
    let mut t = after.trim();
    let closing = t.trim_end_matches('#');
    if closing.is_empty() || closing.ends_with([' ', '\t']) {
        t = closing.trim_end();
    }
    Some((level, t))
}

/// Начало пункта списка: `-`, `*`, `+`, `1.`, `1)` и пробел (или конец строки).
fn list_marker(rest: &str) -> bool {
    let b = rest.as_bytes();
    let digits = b.iter().take_while(|c| c.is_ascii_digit()).count();
    let m = match b.first() {
        Some(b'-' | b'*' | b'+') => 1,
        _ if (1..=9).contains(&digits) && matches!(b.get(digits), Some(b'.' | b')')) => digits + 1,
        _ => return false,
    };
    matches!(b.get(m), None | Some(b' ' | b'\t'))
}

/// Определение ссылки `[метка]: путь ["заголовок"]` (строка без отступа ≥ 4): метка и путь.
fn definition(rest: &str) -> Option<(String, String)> {
    let b = rest.as_bytes();
    if b.first() != Some(&b'[') {
        return None;
    }
    let mut i = 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 1,
            b'[' => return None,
            b']' => break,
            _ => {}
        }
        i += 1;
    }
    if i >= b.len() || b.get(i + 1) != Some(&b':') {
        return None;
    }
    let label = rest[1..i].to_string();
    if label.trim().is_empty() {
        return None;
    }
    let after = rest[i + 2..].trim_start_matches([' ', '\t']);
    let (dest, tail) = if let Some(inner) = after.strip_prefix('<') {
        let end = inner.find('>')?;
        (unescape(&inner[..end]), &inner[end + 1..])
    } else {
        let end = after.find([' ', '\t']).unwrap_or(after.len());
        if end == 0 {
            return None;
        }
        (unescape(&after[..end]), &after[end..])
    };
    // после пути — конец строки или заголовок через пробел
    let title = tail.trim_start_matches([' ', '\t']);
    let spaced = title.len() < tail.len();
    if !title.is_empty() && !(spaced && title.starts_with(['"', '\'', '('])) {
        return None;
    }
    Some((label, dest))
}

/// Метка сноски для сравнения: регистр и пробельные символы не важны (CommonMark).
fn norm_label(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// `\` перед ASCII-пунктуацией снимается (экранирование CommonMark).
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\\'
            && let Some(&n) = it.peek()
            && n.is_ascii_punctuation()
        {
            out.push(n);
            it.next();
            continue;
        }
        out.push(c);
    }
    out
}

/// Первый заголовок `#` документа (вне кода и frontmatter).
fn first_h1(md: &str) -> Option<String> {
    blocks(md).h1
}

/// Назначения ссылок и картинок тела вне кода, формул и frontmatter, как их видит разбор
/// CommonMark сервера: inline `(путь)` и `(<путь>)`, сноски `[текст][метка]`, `[метка][]`,
/// `[метка]` по определениям `[метка]: путь`, автоссылки `<схема:адрес>` и голые автоссылки GFM с
/// недопустимой схемой — `ftp://…`, `xmpp:имя@узел` (файлом не бывают — ради проверки схемы). Экранирование `\` снимается; якорь и схема — как в тексте (их
/// отбрасывает или отклоняет [`resolve_local`]). Вики-ссылки `[[…]]` и сноски `[^1]` — не
/// ссылки на файлы.
pub(crate) fn local_links(md: &str) -> Vec<String> {
    let Blocks { text, defs, .. } = blocks(md);
    Scanner::new(&text, &defs).run()
}

struct Scanner<'a> {
    b: &'a [u8],
    defs: &'a HashMap<String, String>,
    /// Начала пустых строк (граница абзаца для поиска скобок и `` ` ``).
    breaks: Vec<usize>,
    out: Vec<String>,
}

impl<'a> Scanner<'a> {
    fn new(b: &'a [u8], defs: &'a HashMap<String, String>) -> Scanner<'a> {
        let mut breaks = Vec::new();
        let mut line_start = 0;
        let mut only_ws = true;
        for (i, &c) in b.iter().enumerate() {
            match c {
                b'\n' => {
                    if only_ws {
                        breaks.push(line_start);
                    }
                    line_start = i + 1;
                    only_ws = true;
                }
                b' ' | b'\t' | b'\r' => {}
                _ => only_ws = false,
            }
        }
        if only_ws {
            breaks.push(line_start);
        }
        Scanner {
            b,
            defs,
            breaks,
            out: Vec::new(),
        }
    }

    /// Конец абзаца, в котором позиция `i`.
    fn para_end(&self, i: usize) -> usize {
        let k = self.breaks.partition_point(|&s| s <= i);
        self.breaks.get(k).copied().unwrap_or(self.b.len())
    }

    /// Перед позицией `i` в её блоке есть незакрытая `[`: внутри скобок разбор сервера голых
    /// автоссылок `схема://…` и `www.…` не ищет (скобка может оказаться текстом ссылки).
    /// Приближение: скобки считаются по тексту блока как есть (без кода и формул), блок — от
    /// пустой строки или строки с маркером заголовка, списка, цитаты.
    fn in_brackets(&self, i: usize) -> bool {
        let b = self.b;
        let k = self.breaks.partition_point(|&s| s <= i);
        let mut j = if k == 0 { 0 } else { self.breaks[k - 1] };
        let mut depth = 0u32;
        while j < i {
            match b[j] {
                b'\\' => j += 1,
                b'[' => depth += 1,
                b']' => depth = depth.saturating_sub(1),
                b'\n' if block_marker(&b[j + 1..i]) => depth = 0,
                // скобка внутри более ранней голой автоссылки — часть её адреса
                b':' | b'w' if depth == 0 => {
                    let end = if b[j] == b':' {
                        bare_url_autolink(b, j).map(|l| l.end)
                    } else {
                        bare_www_autolink(b, j)
                    };
                    if let Some(end) = end {
                        j = end;
                        continue;
                    }
                }
                _ => {}
            }
            j += 1;
        }
        depth > 0
    }

    fn run(mut self) -> Vec<String> {
        let b = self.b;
        // (откуда, куда): дойдя до `]` текста найденной ссылки, перепрыгнуть её назначение
        let mut jumps: Vec<(usize, usize)> = Vec::new();
        // конец последней голой автоссылки (`схема://…`, `www.…`, почта)
        let mut bare_end = 0;
        let mut i = 0;
        while i < b.len() {
            if let Some(k) = jumps.iter().position(|j| j.0 == i) {
                i = jumps.swap_remove(k).1;
                continue;
            }
            match b[i] {
                b'\\' => {
                    i += if b.get(i + 1).is_some_and(u8::is_ascii_punctuation) {
                        2
                    } else {
                        1
                    }
                }
                b'`' => i = self.skip_code(i),
                b'$' => i = self.skip_math(i),
                b'<' => i = self.skip_angle(i),
                b'!' if b.get(i + 1) == Some(&b'[') => {
                    self.link(i + 1, &mut jumps);
                    i += 2;
                }
                b'[' => {
                    if b.get(i + 1) == Some(&b'[') {
                        // вики-ссылка `[[…]]` — не файл отчёта
                        let end = self.para_end(i);
                        if let Some(k) = find(&b[i..end], b"]]") {
                            i += k + 2;
                            continue;
                        }
                    }
                    self.link(i, &mut jumps);
                    i += 1;
                }
                // голые автоссылки GFM — только ради отказа по схеме (файлом отчёта они не
                // бывают); внутри уже найденной (`https://a/ftp://b`) новых нет
                b':' if i >= bare_end => {
                    if let Some(l) = bare_url_autolink(b, i)
                        && !self.in_brackets(i)
                    {
                        bare_end = l.end;
                        if l.ftp {
                            self.out
                                .push(String::from_utf8_lossy(&b[l.start..l.end]).into_owned());
                        }
                    }
                    i += 1;
                }
                b'w' if i >= bare_end => {
                    if let Some(end) = bare_www_autolink(b, i)
                        && !self.in_brackets(i)
                    {
                        bare_end = end;
                    }
                    i += 1;
                }
                b'@' if i >= bare_end => {
                    if let Some((url, end)) = bare_email_autolink(b, i, bare_end) {
                        bare_end = end;
                        if zam_core::link::url_scheme(&url)
                            .is_some_and(|s| !zam_core::link::report_scheme_allowed(&s))
                        {
                            self.out.push(url);
                        }
                    }
                    i += 1;
                }
                _ => i += 1,
            }
        }
        self.out
    }

    /// После кода `` `…` ``: позиция за закрывающей последовательностью той же длины (нет —
    /// за открывающей).
    fn skip_code(&self, i: usize) -> usize {
        let b = self.b;
        let n = b[i..].iter().take_while(|&&c| c == b'`').count();
        let end = self.para_end(i);
        let mut k = i + n;
        while k < end {
            if b[k] == b'`' {
                let m = b[k..end].iter().take_while(|&&c| c == b'`').count();
                if m == n {
                    return k + m;
                }
                k += m;
            } else {
                k += 1;
            }
        }
        i + n
    }

    /// После формулы `$…$` или `$$…$$` (математика разбора сервера: ссылки внутри не видны).
    fn skip_math(&self, i: usize) -> usize {
        let b = self.b;
        if b.get(i + 1) == Some(&b'$') {
            return find(&b[i + 2..], b"$$").map_or(i + 2, |k| i + 2 + k + 2);
        }
        if b.get(i + 1).is_none_or(|c| c.is_ascii_whitespace()) {
            return i + 1;
        }
        let end = self.para_end(i);
        let mut k = i + 1;
        while k < end {
            match b[k] {
                b'\\' => k += 1,
                b'$' if !b[k - 1].is_ascii_whitespace()
                    && !b.get(k + 1).is_some_and(u8::is_ascii_digit) =>
                {
                    return k + 1;
                }
                _ => {}
            }
            k += 1;
        }
        i + 1
    }

    /// `<…>` — автоссылка или тег HTML: до `>` (ссылки внутри тега не ищутся). Назначение
    /// автоссылки `<схема:адрес>` — в вывод: её схема проверяется как у прочих ссылок.
    fn skip_angle(&mut self, i: usize) -> usize {
        let b = self.b;
        let tag = b
            .get(i + 1)
            .is_some_and(|c| c.is_ascii_alphabetic() || matches!(c, b'/' | b'!' | b'?'));
        if !tag {
            return i + 1;
        }
        let end = self.para_end(i);
        let Some(k) = b[i..end].iter().position(|&c| c == b'>') else {
            return i + 1;
        };
        // `<a:<ftp://x>`: первая `<` — текст, автоссылка — от последней
        let from = b[i..i + k].iter().rposition(|&c| c == b'<').unwrap_or(0);
        let inner = &b[i + from + 1..i + k];
        if is_autolink(inner) {
            self.out.push(String::from_utf8_lossy(inner).into_owned());
        }
        i + k + 1
    }

    /// Ссылка с текстом с позиции `[` (`open`): назначение — в вывод, переход через него — в
    /// `jumps`.
    fn link(&mut self, open: usize, jumps: &mut Vec<(usize, usize)>) {
        let b = self.b;
        let Some(close) = self.match_bracket(open) else {
            return;
        };
        let text = &b[open + 1..close];
        if text.first() == Some(&b'^') {
            // сноска `[^1]` — не ссылка
            return;
        }
        let after = close + 1;
        if b.get(after) == Some(&b'(')
            && let Some((dest, end)) = self.inline_dest(after)
        {
            self.out.push(dest);
            jumps.push((close, end));
            return;
        }
        if b.get(after) == Some(&b'[')
            && let Some(lc) = self.label_close(after)
        {
            let label = &b[after + 1..lc];
            let label = if label.is_empty() { text } else { label };
            if let Some(d) = self.def(label) {
                self.out.push(d);
                jumps.push((close, lc + 1));
            }
            return;
        }
        if let Some(d) = self.def(text) {
            self.out.push(d);
        }
    }

    fn def(&self, label: &[u8]) -> Option<String> {
        let l = std::str::from_utf8(label).ok()?;
        self.defs.get(&norm_label(l)).cloned()
    }

    /// Закрывающая `]` метки сноски (без вложенных скобок).
    fn label_close(&self, open: usize) -> Option<usize> {
        let b = self.b;
        let end = self.para_end(open);
        let mut k = open + 1;
        while k < end {
            match b[k] {
                b'\\' => k += 1,
                b'[' => return None,
                b']' => return Some(k),
                _ => {}
            }
            k += 1;
        }
        None
    }

    /// Парная `]` текста ссылки (вложенные скобки, экранирование, код).
    fn match_bracket(&self, open: usize) -> Option<usize> {
        let b = self.b;
        let end = self.para_end(open);
        let mut depth = 0usize;
        let mut k = open;
        while k < end {
            match b[k] {
                b'\\' => k += 1,
                b'`' => {
                    k = self.skip_code(k);
                    continue;
                }
                b'[' => depth += 1,
                b']' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(k);
                    }
                }
                _ => {}
            }
            k += 1;
        }
        None
    }

    /// Назначение inline-ссылки с `(` в `p`: путь (экранирование снято) и позиция за `)`.
    fn inline_dest(&self, p: usize) -> Option<(String, usize)> {
        let b = self.b;
        let end = self.para_end(p);
        let mut j = skip_ws(b, p + 1, end);
        let dest;
        if b.get(j) == Some(&b'<') {
            let s = j + 1;
            let mut k = s;
            while k < end {
                match b[k] {
                    b'\\' => k += 1,
                    b'>' => break,
                    b'<' | b'\n' => return None,
                    _ => {}
                }
                k += 1;
            }
            if k >= end {
                return None;
            }
            dest = unescape(std::str::from_utf8(&b[s..k]).ok()?);
            j = k + 1;
        } else {
            let s = j;
            let mut depth = 0usize;
            while j < end {
                match b[j] {
                    b'\\' => j += 1,
                    b'(' => depth += 1,
                    b')' if depth == 0 => break,
                    b')' => depth -= 1,
                    c if c.is_ascii_whitespace() || c.is_ascii_control() => break,
                    _ => {}
                }
                j += 1;
            }
            if depth != 0 {
                return None;
            }
            dest = unescape(std::str::from_utf8(&b[s..j.min(end)]).ok()?);
        }
        // заголовок ссылки — только через пробел после пути
        let k = skip_ws(b, j, end);
        let close = match b.get(k) {
            Some(b'"') if k > j => Some(b'"'),
            Some(b'\'') if k > j => Some(b'\''),
            Some(b'(') if k > j => Some(b')'),
            _ => None,
        };
        let j = match close {
            Some(c) => {
                let mut t = k + 1;
                while t < end && b[t] != c {
                    if b[t] == b'\\' {
                        t += 1;
                    }
                    t += 1;
                }
                if t >= end {
                    return None;
                }
                skip_ws(b, t + 1, end)
            }
            None => k,
        };
        (b.get(j) == Some(&b')')).then_some((dest, j + 1))
    }
}

/// Позиция после пробелов и не более одного перевода строки.
fn skip_ws(b: &[u8], mut i: usize, end: usize) -> usize {
    let mut nl = false;
    while i < end {
        match b[i] {
            b' ' | b'\t' | b'\r' => i += 1,
            b'\n' if !nl => {
                nl = true;
                i += 1;
            }
            _ => break,
        }
    }
    i
}

/// Содержимое `<…>` — автоссылка CommonMark с адресом: схема из 2–32 символов (буква, затем
/// буквы, цифры, `+.-`), `:`, дальше без пробелов, управляющих символов и `<`.
fn is_autolink(s: &[u8]) -> bool {
    let Some(colon) = s.iter().position(|&c| c == b':') else {
        return false;
    };
    (2..=32).contains(&colon)
        && s[0].is_ascii_alphabetic()
        && s[1..colon]
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || b"+.-".contains(c))
        && !s.iter().any(|&c| c <= b' ' || c == b'<')
}

/// Знак годится для имени узла голой автоссылки GFM — как `is_valid_hostchar` разбора сервера
/// (comrak): не пробельный, не знак препинания и не символ Юникода. Для ASCII — точно (буква,
/// цифра или непробельный управляющий); вне ASCII таблиц категорий у клиента нет — отсекаются
/// только частые знаки (кавычки, тире, многоточие, стрелки, полноширинная пунктуация), прочее
/// считается буквой. Ошибка приближения — только лишний отказ клиента или отказ сервера `422`
/// вместо отказа клиента.
fn host_char(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_alphanumeric() || (c.is_ascii_control() && !c.is_whitespace());
    }
    !(c.is_whitespace()
        || matches!(
            c,
            '\u{A1}'..='\u{BF}'
                | '\u{D7}'
                | '\u{F7}'
                | '\u{2010}'..='\u{2027}'
                | '\u{2030}'..='\u{205E}'
                | '\u{2190}'..='\u{23FF}'
                | '\u{2500}'..='\u{27BF}'
                | '\u{3001}'..='\u{3003}'
                | '\u{3008}'..='\u{3011}'
                | '\u{FF01}'..='\u{FF0F}'
        ))
}

/// Имя узла голой автоссылки GFM принимается — `check_domain` разбора сервера: узел — знаки
/// [`host_char`], `-`, `_`, `.`; в двух последних метках нет `_` (если точек не больше десяти);
/// `short` — точка в узле не обязательна (`схема://узел`; у `www.` — обязательна).
fn bare_domain_ok(tail: &str, short: bool) -> bool {
    let (mut dots, mut underscores, mut prev_underscores) = (0, 0, 0);
    for (k, c) in tail.char_indices() {
        match c {
            '\\' if k + 1 < tail.len() => {}
            '_' => underscores += 1,
            '.' => {
                prev_underscores = underscores;
                underscores = 0;
                dots += 1;
            }
            c if host_char(c) || c == '-' => {}
            _ => break,
        }
    }
    dots > 10 || (underscores == 0 && prev_underscores == 0 && (short || dots > 0))
}

/// Строка начинает новый блок: маркер заголовка, цитаты или пункта списка.
fn block_marker(line: &[u8]) -> bool {
    let line = &line[line.iter().take_while(|&&c| c == b' ').count()..];
    match line.first() {
        Some(b'#' | b'>') => true,
        Some(b'-' | b'*' | b'+') => line.get(1).is_none_or(|&c| c == b' '),
        Some(c) if c.is_ascii_digit() => {
            let n = line.iter().take_while(|c| c.is_ascii_digit()).count();
            matches!(line.get(n), Some(b'.' | b')')) && line.get(n + 1).is_none_or(|&c| c == b' ')
        }
        _ => false,
    }
}

/// Пробельный символ ASCII разбора сервера (`isspace`).
fn bare_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// Конец голой автоссылки, начатой до `from`: первый пробельный символ или `<`.
fn bare_link_end(b: &[u8], from: usize) -> usize {
    b[from..]
        .iter()
        .position(|&c| bare_space(c) || c == b'<')
        .map_or(b.len(), |k| from + k)
}

/// Голая автоссылка `схема://…`: байты `start..end` текста.
struct BareUrl {
    start: usize,
    end: usize,
    /// Схема `ftp` — её сервер отклоняет; иначе `http`/`https`.
    ftp: bool,
}

/// Голая автоссылка GFM `схема://узел…` (расширение `autolink` разбора сервера, `url_match`
/// comrak 0.56) по двоеточию в позиции `i`. Ссылку `ftp://…` сервер отклоняет по схеме;
/// `http(s)://…` разрешена, но нужна её граница: всё до пробельного символа — её адрес, новых
/// ссылок внутри нет. Правило снято с разбора сервера:
/// - за двоеточием `//`, перед ним — буквы ASCII, целиком равные `http`, `https` или `ftp` без
///   учёта регистра (`словоftp://…` и `1ftp://…` — ссылки, `aftp://…`, `sftp://…` — нет);
/// - первый знак узла — [`host_char`], узел — [`bare_domain_ok`].
fn bare_url_autolink(b: &[u8], i: usize) -> Option<BareUrl> {
    if b.get(i + 1) != Some(&b'/') || b.get(i + 2) != Some(&b'/') {
        return None;
    }
    let start = i - b[..i]
        .iter()
        .rev()
        .take_while(|c| c.is_ascii_alphabetic())
        .count();
    let scheme = &b[start..i];
    let ftp = scheme.eq_ignore_ascii_case(b"ftp");
    if !(ftp || scheme.eq_ignore_ascii_case(b"http") || scheme.eq_ignore_ascii_case(b"https")) {
        return None;
    }
    let end = bare_link_end(b, i + 3);
    let tail = String::from_utf8_lossy(&b[i + 3..end]);
    (tail.chars().next().is_some_and(host_char) && bare_domain_ok(&tail, true)).then_some(BareUrl {
        start,
        end,
        ftp,
    })
}

/// Голая автоссылка GFM `www.узел.зона…` (`www_match` comrak 0.56) с позиции `i`: её конец. Схема
/// (`http`) разрешена — нужна только граница, как у [`bare_url_autolink`]. Перед `www.` — начало
/// текста, пробельный символ или один из `*_~([`.
fn bare_www_autolink(b: &[u8], i: usize) -> Option<usize> {
    let before = i == 0 || bare_space(b[i - 1]) || b"*_~([".contains(&b[i - 1]);
    if !before || !b[i..].starts_with(b"www.") {
        return None;
    }
    let end = bare_link_end(b, i + 4);
    bare_domain_ok(&String::from_utf8_lossy(&b[i + 4..end]), false).then_some(end)
}

/// Голая автоссылка почты GFM (`имя@узел.зона`, `xmpp:имя@узел.зона`; `email_match` comrak 0.56)
/// по знаку `@` в позиции `i`: её адрес и конец в тексте. С протоколом адрес — текст от начала
/// имени, вместе со всем, что стоит перед протоколом слитно (`a.xmpp:имя@узел.зона` — схема
/// `a.xmpp`), поэтому решает [`zam_core::link::url_scheme`] у вызывающего; без протокола сервер
/// подставляет `mailto:`. `floor` — конец предыдущей голой автоссылки: её текст в новую не входит.
/// Правило снято с разбора сервера:
/// - имя — назад по буквам, цифрам и `.+-_`; двоеточие входит, если перед ним буквы ASCII,
///   целиком равные `mailto` или `xmpp` (регистр важен);
/// - узел — вперёд по буквам, цифрам, `-`, `_`, точке перед буквой или цифрой (нужна хотя бы
///   одна), у `xmpp` ещё `/`; второй `@` — не адрес; последний знак — буква или точка;
/// - `@` внутри незакрытой `[` — не адрес (глубина скобок считается по строке — приближение:
///   разбор сервера считает её по текстовому узлу); текст адреса кончается там, где дальше в
///   строке начинается голая автоссылка `схема://…` (она разбирается раньше почты).
fn bare_email_autolink(b: &[u8], i: usize, floor: usize) -> Option<(String, usize)> {
    let line = b[..i]
        .iter()
        .rposition(|&c| c == b'\n')
        .map_or(0, |k| k + 1)
        .max(floor);
    let mut end = b[i..]
        .iter()
        .position(|&c| c == b'\n')
        .map_or(b.len(), |k| i + k);
    if let Some(next) = (i + 1..end)
        .filter(|&k| b[k] == b':')
        .find_map(|k| bare_url_autolink(b, k))
    {
        end = end.min(next.start.max(i + 1));
    }
    let depth: i32 = b[line..i]
        .iter()
        .map(|&c| match c {
            b'[' => 1,
            b']' => -1,
            _ => 0,
        })
        .sum();
    if depth > 0 {
        return None;
    }
    let protocol = |name: &[u8], colon: usize| {
        let from = colon
            - b[line..colon]
                .iter()
                .rev()
                .take_while(|c| c.is_ascii_alphabetic())
                .count();
        &b[from..colon] == name
    };
    let (mut start, mut with_protocol, mut xmpp) = (i, false, false);
    while start > line {
        let c = b[start - 1];
        if c.is_ascii_alphanumeric() || b".+-_".contains(&c) {
            start -= 1;
        } else if c == b':' && protocol(b"mailto", start - 1) {
            with_protocol = true;
            start -= 1;
        } else if c == b':' && protocol(b"xmpp", start - 1) {
            (with_protocol, xmpp) = (true, true);
            start -= 1;
        } else {
            break;
        }
    }
    if start == i {
        return None;
    }
    let size = end - i;
    let (mut len, mut dots) = (1, 0);
    while len < size {
        let c = b[i + len];
        if c.is_ascii_alphanumeric() {
        } else if c == b'@' {
            return None;
        } else if c == b'.' && len < size - 1 && b[i + len + 1].is_ascii_alphanumeric() {
            dots += 1;
        } else if !(c == b'-' || c == b'_' || (c == b'/' && xmpp)) {
            break;
        }
        len += 1;
    }
    let last = b[i + len - 1];
    if len < 2 || dots == 0 || !(last.is_ascii_alphabetic() || last == b'.') {
        return None;
    }
    let text = String::from_utf8_lossy(&b[start..i + len]);
    let url = if with_protocol {
        text.into_owned()
    } else {
        format!("mailto:{text}")
    };
    Some((url, i + len))
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Локальные назначения (без схемы, якорей и `//`), путь без якоря.
    fn local(v: Vec<String>) -> Vec<String> {
        let mut v: Vec<String> = v
            .into_iter()
            .filter(|t| !t.is_empty() && !t.starts_with('#') && !t.starts_with("//"))
            .filter(|t| zam_core::link::url_scheme(t).is_none())
            .map(|t| t.split('#').next().unwrap_or_default().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        v.sort();
        v
    }

    fn server(md: &str) -> Vec<String> {
        local(
            zam_markdown::parse(md)
                .links
                .into_iter()
                .filter(|l| l.kind != zam_markdown::LinkKind::Wiki)
                .map(|l| l.target)
                .collect(),
        )
    }

    /// Согласие сканера CLI с разбором сервера (comrak) на корпусе форм ссылок.
    #[test]
    fn local_links_agrees_with_zam_markdown() {
        let corpus = [
            "# T\n\n![a](figs/a.png) и [b](data/b.csv \"заголовок\")\n",
            "![a](<figs/my plot.png>) ![b](figs/my%20plot.png) ![c](./figs/c.png)\n",
            "```\n![code](in/fence.png)\n```\n\n~~~md\n[x](in/tilde.md)\n~~~\n![out](out.png)\n",
            "Текст\n\n    ![indented](in/code.png)\n\nабзац ![ok](ok.png)\n",
            "Абзац\n    ![lazy](lazy.png)\n",
            "`![span](in/span.png)` и ``a ` [b](in/b.png) `` и ![ok](ok2.png)\n",
            "[ref][r1], [R1][], [r1] и ![img][pic]\n\n[r1]: docs/r1.pdf\n[pic]: <figs/pic 1.png> \"t\"\n",
            "Не определено [nope] и [x][nope2]\n\n[unused]: un/used.png\n",
            "\\[not](link.png) \\![no](img.png) [esc](figs/a\\_b.png)\n",
            "[a [nested] text](nested.png) и [![inner](in.png)](outer.html)\n",
            "[p](path(with)parens.png) [q](<a>b.png>)\n",
            "Сноска[^1].\n\n[^1]: см. [данные](data/fn.csv)\n",
            "[[Вики]] и [[Вики|текст]] и ![a](w.png)\n",
            "<https://example.com> <img src=\"x.png\"> [h](https://e.com/a.png) [m](mailto:a@b.c)\n",
            "[якорь](#sec) [файл с якорем](doc.md#part)\n",
            "$[a](in/math.png)$ и $$\n[b](in/math2.png)\n$$ и ![ok](ok3.png) за $5 и $10\n",
            "---\ntitle: x\nlink: \"[f](in/front.png)\"\n---\n\n![ok](ok4.png)\n",
            "- пункт\n\n    ![in list](list.png)\n\n1. a\n   [b](list2.md)\n",
            "| a | b |\n|---|---|\n| ![t](table.png) | [c](c.csv) |\n",
            "> цитата ![q](quote.png)\n",
            "![a](figs/a.png)\n![a](figs/a.png)\n",
            "[multi\nline](multi.png) [x](\nnext.png)\n",
        ];
        let mut total = 0;
        for md in corpus {
            let want = server(md);
            total += want.len();
            assert_eq!(local(local_links(md)), want, "корпус:\n{md}");
        }
        // корпус не пустой по сути: разбор сервера видит ссылки почти в каждом примере
        assert!(total >= 30, "{total}");
        // контрольные точки трудных форм
        assert_eq!(
            local(local_links(corpus[15])),
            ["ok3.png"],
            "формулы — без ссылок"
        );
        assert_eq!(local(local_links(corpus[16])), ["ok4.png"], "frontmatter");
        assert_eq!(
            local(local_links(corpus[6])),
            [
                "docs/r1.pdf",
                "docs/r1.pdf",
                "docs/r1.pdf",
                "figs/pic 1.png"
            ],
            "сноски"
        );
    }

    /// Автоссылка `<схема:адрес>` попадает в назначения (её схему проверяет `local_name`);
    /// тег HTML, почта и `<…>` с пробелом — нет.
    #[test]
    fn autolinks_are_scanned_for_scheme_check() {
        let md = "<ftp://e.org/a> и <https://e.org/b> и <FILE:///etc/hosts>\n\n\
                  <a href=\"x:y\">тег</a> <b> <a@b.c> <не ссылка: текст> <c:\\x> `<javascript:x>`\n";
        assert_eq!(
            local_links(md),
            ["ftp://e.org/a", "https://e.org/b", "FILE:///etc/hosts"]
        );
        assert!(local_name("ftp://e.org/a").is_err());
        assert!(local_name("https://e.org/b").unwrap().is_none());
        assert!(local_name("FILE:///etc/hosts").unwrap().is_none());
        for (s, auto) in [
            ("ab:c", true),
            ("a:c", false),
            ("ab", false),
            ("1b:c", false),
            ("ab:c d", false),
            ("ab:c<d", false),
            ("a-b+c.d:x", true),
        ] {
            assert_eq!(is_autolink(s.as_bytes()), auto, "{s}");
        }
    }

    /// Ревью 15a, Minor 5: всё, что разбор сервера (comrak с расширением `autolink`) считает
    /// ссылкой с недопустимой схемой, сканер CLI тоже видит и отклоняет — и голые `ftp://…`,
    /// `xmpp:…` в тексте; а где сервер ссылки не видит (код, середина слова) — не отклоняет.
    #[test]
    fn rejected_schemes_agree_with_server_parse() {
        let rejected = |targets: Vec<String>| -> Vec<String> {
            let mut v: Vec<String> = targets
                .iter()
                .filter_map(|t| zam_core::link::url_scheme(t))
                .filter(|s| !zam_core::link::report_scheme_allowed(s))
                .collect();
            v.sort();
            v.dedup();
            v
        };
        let corpus: [(&str, &[&str]); 14] = [
            ("см. ftp://example.org/a тут\n", &["ftp"]),
            ("ftp://example.org/a в начале\n", &["ftp"]),
            ("(ftp://example.org/a) и *ftp://example.org/b*\n", &["ftp"]),
            ("FTP://EXAMPLE.ORG/a\n", &["ftp"]),
            ("пишите xmpp:user@example.org сюда\n", &["xmpp"]),
            (
                "<ftp://example.org/a> и [x](ftp://example.org/b)\n",
                &["ftp"],
            ),
            ("сайт https://example.org и www.example.org\n", &[]),
            (
                "`ftp://example.org/a` в коде\n\n```\nftp://example.org/b\n```\n",
                &[],
            ),
            ("словоftp://example.org/a слитно\n", &["ftp"]),
            ("aftp://example.org/a и sftp://example.org/b\n", &[]),
            ("1ftp://example.org/b\n", &["ftp"]),
            ("ftp:/example.org и ftp: текст и xmpp: текст\n", &[]),
            (
                "[x](javascript:alert(1)) и ftp://example.org/a\n",
                &["ftp", "javascript"],
            ),
            (
                "- пункт ftp://example.org/a\n\n> цитата ftp://example.org/b\n",
                &["ftp"],
            ),
        ];
        for (md, want) in corpus {
            let server = rejected(
                zam_markdown::parse(md)
                    .links
                    .into_iter()
                    .filter(|l| l.kind != zam_markdown::LinkKind::Wiki)
                    .map(|l| l.target)
                    .collect(),
            );
            assert_eq!(server, want, "разбор сервера:\n{md}");
            assert_eq!(rejected(local_links(md)), want, "сканер CLI:\n{md}");
        }
    }

    /// Ревью 15a, Minor 5: то же согласие на случайном тексте (фиксированное зерно) — голые
    /// автоссылки среди слов, знаков препинания, переводов строк и выделения. Сравнивается
    /// решение «отклонить» по набору схем: сканер CLI и разбор сервера.
    ///
    /// Чего в наборе кусков нет намеренно: одиночного `_` (выделение `_xmpp:имя@узел_` делит
    /// текст на узлы, и адрес начинается с протокола; сканер границ выделения не знает и видит
    /// `_xmpp:…` — относительный адрес; такой отчёт отклонит сервер, `422`).
    #[test]
    fn bare_autolinks_agree_with_server_parse_on_random_text() {
        #[rustfmt::skip]
        const PARTS: &[&str] = &[
            "ftp://", "FTP://", "Ftp://", "xmpp:", "XMPP:", "ftp:", "ftp:/", "example.org/a",
            "user@example.org", "host", "a", "z", "я", "1", " ", " ", "\n", "\n\n", "(", ")", "*",
            "~", ".", ",", "-", "/", ":", "@", "\"", "'", "!", "?", "www.", "https://", "> ", "- ",
            "# ", "    ", "|", "mailto:", "h.org", "u@h.org/r", "+", "a.", "_b", "a_b", "[", "]",
            "`", "<", ">", "«", "…",
        ];
        let rejected = |targets: Vec<String>| -> Vec<String> {
            let mut v: Vec<String> = targets
                .iter()
                .filter_map(|t| zam_core::link::url_scheme(t))
                .filter(|s| !zam_core::link::report_scheme_allowed(s))
                .collect();
            v.sort();
            v.dedup();
            v
        };
        let mut x: u32 = 0x15A_2026;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x
        };
        let (mut with_link, mut diff) = (0, Vec::new());
        for _ in 0..60_000 {
            let n = 1 + next() % 9;
            let mut md = String::from("т ");
            for _ in 0..n {
                md.push_str(PARTS[(next() % PARTS.len() as u32) as usize]);
            }
            md.push('\n');
            let server = rejected(
                zam_markdown::parse(&md)
                    .links
                    .into_iter()
                    .filter(|l| l.kind != zam_markdown::LinkKind::Wiki)
                    .map(|l| l.target)
                    .collect(),
            );
            with_link += usize::from(!server.is_empty());
            let cli = rejected(local_links(&md));
            if server != cli {
                diff.push(format!("{md:?}: сервер {server:?}, CLI {cli:?}"));
            }
        }
        assert!(with_link > 300, "{with_link}");
        assert!(
            diff.is_empty(),
            "{} расхождений:\n{}",
            diff.len(),
            diff[..diff.len().min(40)].join("\n")
        );
    }

    #[test]
    fn names_and_rules() {
        assert_eq!(
            local_name("figs/a%20b.png").unwrap().unwrap(),
            "figs/a b.png"
        );
        assert_eq!(local_name("././a.png#x").unwrap().unwrap(), "a.png");
        for none in [
            "https://e.com/a.png",
            "http://e.com",
            "mailto:a@b.c",
            "vscode://file/C:/proj/a.rs:10",
            "file:///C:/data/a.csv",
            "FILE:///etc/hosts",
            "#sec",
            "//host/a.png",
            "",
        ] {
            assert!(local_name(none).unwrap().is_none(), "{none}");
        }
        // схема не из общего списка — ошибка ввода с перечнем разрешённых (M6 §16 п. 15)
        for (bad, scheme) in [
            ("ftp://e.com/a", "ftp"),
            ("javascript:alert(1)", "javascript"),
            ("JaVaScRiPt:alert(1)", "javascript"),
            (" javascript:alert(1)", "javascript"),
            ("java\tscript:alert(1)", "javascript"),
            ("data:image/png;base64,AAAA", "data"),
            ("vscode-insiders://file/x", "vscode-insiders"),
        ] {
            let e = local_name(bad).unwrap_err();
            assert!(
                e.msg.contains(&format!("схема {scheme}:"))
                    && e.msg.contains("http, https, mailto, vscode, file"),
                "{bad}: {}",
                e.msg
            );
        }
        for bad in [
            "../a.png",
            "figs/../../a.png",
            "/etc/passwd",
            "C:/x.png",
            "c:\\x.png",
            "figs\\a.png",
            "figs%5Ca.png",
            "%2Fetc",
            "a\"b.png",
            "a%ZZ.png",
            "a//b.png",
            "_attachments/0123456789abcdef/x.png",
            "src/",
        ] {
            assert!(local_name(bad).is_err(), "{bad}");
        }
        let e = local_name("src/").unwrap_err();
        assert!(e.msg.contains("каталог"), "{}", e.msg);
        // пределы пути узла сервера: сегмент 255 байт, путь 1024
        let long = format!("figs/{}.png", "я".repeat(130));
        assert!(local_name(&long).is_err());
        let deep = format!("{}a.png", "abcdefghi/".repeat(103));
        assert!(local_name(&deep).is_err());
        assert!(
            local_name(&format!("{}.png", "a".repeat(251)))
                .unwrap()
                .is_some()
        );
    }

    /// Файл изменился между `stat` и отправкой: короче или длиннее — ошибка чтения тела.
    #[test]
    fn lazy_file_detects_size_change() {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-tmp/cli-report-lazy");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.bin");
        std::fs::write(&f, b"0123456789").unwrap();
        let mut out = Vec::new();
        LazyFile::new(f.clone(), 10).read_to_end(&mut out).unwrap();
        assert_eq!(out, b"0123456789");
        let e = LazyFile::new(f.clone(), 8)
            .read_to_end(&mut Vec::new())
            .unwrap_err();
        assert!(e.to_string().contains("вырос"), "{e}");
        let e = LazyFile::new(f, 12)
            .read_to_end(&mut Vec::new())
            .unwrap_err();
        assert!(e.to_string().contains("короче"), "{e}");
    }

    #[test]
    fn title_from_first_h1() {
        assert_eq!(
            first_h1("x\n\n## B\n# Итог  ##\n# Второй\n").as_deref(),
            Some("Итог")
        );
        assert_eq!(first_h1("```\n# в коде\n```\n## только h2\n"), None);
        assert_eq!(
            first_h1("---\ntitle: a\n---\n# После\n").as_deref(),
            Some("После")
        );
        assert_eq!(first_h1("#хештег\n"), None);
    }

    #[test]
    fn multipart_layout_and_length() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-tmp/cli-report-mp");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.png");
        std::fs::write(&f, b"PNGDATA").unwrap();
        let (ct, len, mut r) = report_multipart(
            "B",
            &json!({"title": "T"}),
            &[("figs/рис 1.png".into(), f, 7)],
        );
        assert_eq!(ct, "multipart/form-data; boundary=B");
        let mut body = Vec::new();
        r.read_to_end(&mut body).unwrap();
        assert_eq!(body.len() as u64, len);
        let s = String::from_utf8(body).unwrap();
        assert!(
            s.starts_with("--B\r\nContent-Disposition: form-data; name=\"meta\"\r\n"),
            "{s}"
        );
        assert!(
            s.contains(
                "filename=\"figs/рис 1.png\"; filename*=UTF-8''figs%2F%D1%80%D0%B8%D1%81%201.png\r\n"
            ),
            "{s}"
        );
        assert!(s.contains("\r\n\r\nPNGDATA\r\n--B--\r\n"), "{s}");
    }
}
