//! Ссылки знаний на файлы решения на стороне клиента (M4.5 §6): путь, хэш, коммит и команды
//! `zam refs check|verify`.
//!
//! - **Путь** ([`ref_path`]) — относительно корня проекта ([`super::hook::project_root`], как у
//!   `pre-tool`), `/`, лексически нормализован по правилам сервера ([`normalize_ref_path`] —
//!   копия `zam_store::file_refs::normalize_ref_path`: `zam-cli` не зависит от `zam-store`,
//!   совпадение проверяет тест `ref_path_rules_match_store`). Сверх правил сервера на Windows
//!   ([`local_rules`]): у сегментов срезаются хвостовые точки и пробелы, имена устройств (`CON`,
//!   `NUL`, `COM1`, …) и `:` в сегменте (альтернативный поток NTFS) отклоняются до открытия
//!   файла, а регистр берётся из файловой системы ([`real_case`]); сравнение путей — без учёта
//!   регистра ([`same_path`]). Так одна и та же ссылка с Windows и Linux не расходится.
//! - **Хэш** ([`hash_file`]) — blake3 сырого содержимого, 64 hex; только обычный файл.
//! - **Коммит** ([`git_head`]) — `git rev-parse HEAD`, если над корнем есть `.git` и `git` в
//!   `PATH`; ошибка — без коммита.
//! - `zam refs check [--since <commit>]` — пути ссылок решения (`GET /file-refs`) сверяются с
//!   рабочей копией; с `--since` набор сужают `git diff --name-only <c>..HEAD` и
//!   `git status --porcelain`. Изменившиеся (и ещё без ориентира) уходят `POST /files/changed`
//!   пачками до [`FILES_MAX`]; удалённый файл — хэш [`MISSING_HASH`] **[решение исполнителя]**.
//! - `zam refs verify <path>…` → `POST /files/verified` («проверено, знание актуально»).
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

use super::{Ctx, emit, emit_json, home_dir, hook, solution_guid};
use crate::client::{CliError, read_json};

/// Предел длины пути ссылки, символов (как у сервера).
pub const PATH_MAX_CHARS: usize = 1000;
/// Предел файлов в одном `POST /files/changed|verified` (как у сервера).
pub const FILES_MAX: usize = 500;
/// Предел ссылок одной записи (как у сервера).
pub const REFS_MAX: usize = 64;
/// «Хэш» удалённого файла в `POST /files/changed`: не совпадёт ни с одним содержимым, ссылка
/// становится `stale` **[решение исполнителя, задача 8]**.
pub const MISSING_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// Путь ссылки не принят.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathError {
    Empty,
    TooLong,
    Control,
    Absolute,
    AboveRoot,
    /// Имя устройства Windows (`CON`, `NUL`, `COM1`, …).
    Device,
    /// `:` в имени — альтернативный поток NTFS.
    Stream,
    /// Путь вне корня проекта.
    Outside,
}

impl std::fmt::Display for PathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PathError::Empty => "пустой путь",
            PathError::TooLong => "путь длиннее 1000 символов",
            PathError::Control => "управляющие или невидимые символы в пути",
            PathError::Absolute => "абсолютный путь: нужен путь относительно корня решения",
            PathError::AboveRoot => "путь выходит выше корня решения",
            PathError::Device => "имя устройства Windows (CON, NUL, COM1…)",
            PathError::Stream => "двоеточие в имени (альтернативный поток NTFS)",
            PathError::Outside => "файл вне корня проекта",
        })
    }
}

/// Нормализация пути по правилам сервера (копия `zam_store::file_refs::normalize_ref_path`):
/// края без пробелов, `\` → `/`, пустые сегменты и `.` убираются, `..` сворачивается;
/// абсолютный путь (`/x`, `\\server`, `C:\x`, сегмент `C:` где угодно), выход выше корня,
/// пустой итог, длина больше [`PATH_MAX_CHARS`] и управляющие/невидимые символы — ошибка.
pub fn normalize_ref_path(p: &str) -> Result<String, PathError> {
    let p = p.trim();
    if p.is_empty() {
        return Err(PathError::Empty);
    }
    if p.chars().count() > PATH_MAX_CHARS {
        return Err(PathError::TooLong);
    }
    if p.chars().any(|c| c.is_control() || is_invisible(c)) {
        return Err(PathError::Control);
    }
    let p = p.replace('\\', "/");
    if p.starts_with('/') {
        return Err(PathError::Absolute);
    }
    let mut out: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if out.pop().is_none() {
                    return Err(PathError::AboveRoot);
                }
            }
            s if is_drive(s) => return Err(PathError::Absolute),
            s => out.push(s),
        }
    }
    if out.is_empty() {
        return Err(PathError::Empty);
    }
    let norm = out.join("/");
    if norm.starts_with('/') || is_drive(&norm) {
        return Err(PathError::Absolute);
    }
    Ok(norm)
}

/// Сегмент начинается с буквы диска: `C:`, `c:x`.
fn is_drive(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

/// Невидимые символы форматирования (как у сервера): bidi-управляющие и нулевой ширины.
fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{061C}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
    )
}

/// Имя устройства Windows: основа до первой точки (без хвостовых пробелов) — `CON`, `PRN`,
/// `AUX`, `NUL`, `COM0`–`COM9`, `LPT0`–`LPT9` (и с надстрочными ¹²³), `CONIN$`, `CONOUT$`.
fn is_device(seg: &str) -> bool {
    let stem = seg
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_ascii_uppercase();
    if matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return true;
    }
    let mut cs = stem.chars();
    let head: String = cs.by_ref().take(3).collect();
    let tail: Vec<char> = cs.collect();
    (head == "COM" || head == "LPT")
        && tail.len() == 1
        && (tail[0].is_ascii_digit() || matches!(tail[0], '¹' | '²' | '³'))
}

/// Правила Windows сверх серверных (`windows` — платформа; параметр — ради тестов на любой
/// ОС): хвостовые точки и пробелы сегментов срезаются (кроме `.` и `..`), имя устройства и
/// `:` в сегменте — ошибка. На других ОС путь не меняется.
pub fn local_rules(p: &str, windows: bool) -> Result<String, PathError> {
    if !windows {
        return Ok(p.to_string());
    }
    let p = p.replace('\\', "/");
    let mut segs: Vec<String> = Vec::new();
    for (i, seg) in p.split('/').enumerate() {
        if seg.chars().all(|c| c == '.') {
            segs.push(seg.to_string());
            continue;
        }
        let s = seg.trim_end_matches(['.', ' ']);
        if s.is_empty() {
            return Err(PathError::Empty);
        }
        if is_device(s) {
            return Err(PathError::Device);
        }
        // `C:` в начале пути — абсолютный путь (ответит normalize_ref_path), остальное — поток
        if s.contains(':') && !(i == 0 && is_drive(s)) {
            return Err(PathError::Stream);
        }
        segs.push(s.to_string());
    }
    Ok(segs.join("/"))
}

/// Пути равны: на Windows — без учёта регистра.
pub fn same_path(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        a == b || a.to_lowercase() == b.to_lowercase()
    } else {
        a == b
    }
}

/// Регистр пути из файловой системы (Windows): канонический путь файла относительно
/// канонического корня, если он отличается от `rel` только регистром (симлинк дал бы другой
/// путь — тогда `rel` как есть). Файла нет — `rel`.
pub fn real_case(root: &Path, rel: &str) -> String {
    if !cfg!(windows) {
        return rel.to_string();
    }
    let (Ok(abs), Ok(r)) = (
        std::fs::canonicalize(root.join(rel)),
        std::fs::canonicalize(root),
    ) else {
        return rel.to_string();
    };
    let (a, r) = (
        abs.to_string_lossy().replace('\\', "/"),
        r.to_string_lossy().replace('\\', "/"),
    );
    let r = r.trim_end_matches('/');
    let Some(rest) = strip_prefix_ci(&a, r) else {
        return rel.to_string();
    };
    let rest = rest.trim_start_matches('/');
    if rest.to_lowercase() == rel.to_lowercase() {
        rest.to_string()
    } else {
        rel.to_string()
    }
}

/// `strip_prefix` без учёта регистра.
fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let mut it = s.char_indices();
    for pc in prefix.chars() {
        let (_, c) = it.next()?;
        if !c.to_lowercase().eq(pc.to_lowercase()) {
            return None;
        }
    }
    Some(it.next().map_or("", |(i, _)| &s[i..]))
}

/// Путь ссылки для файла `file` (абсолютный или относительно `cwd`): относительно `root`,
/// по правилам сервера и Windows, в регистре файловой системы.
pub fn ref_path(file: &str, cwd: &Path, root: &Path) -> Result<String, PathError> {
    let file = file.trim();
    if file.is_empty() {
        return Err(PathError::Empty);
    }
    if file.chars().any(|c| c.is_control() || is_invisible(c)) {
        return Err(PathError::Control);
    }
    // правила Windows — до сопоставления с корнем: «a.» и «a» — один файл, CON не открывать
    let local = local_rules(file, cfg!(windows))?;
    let rel = hook::pre_tool::glob_path(&local, cwd, root).ok_or(PathError::Outside)?;
    let norm = normalize_ref_path(&rel)?;
    normalize_ref_path(&real_case(root, &norm))
}

/// blake3 сырого содержимого обычного файла, 64 hex.
pub fn hash_file(path: &Path) -> std::io::Result<String> {
    let meta = std::fs::metadata(path)?;
    if !meta.is_file() {
        return Err(std::io::Error::other("не обычный файл"));
    }
    let mut f = std::fs::File::open(path)?;
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().to_hex().to_string())
}

/// Хэш файла пути ссылки `rel` под корнем `root`: `None` — файла нет (или не обычный файл).
pub fn hash_rel(root: &Path, rel: &str) -> Option<String> {
    hash_file(&root.join(rel)).ok()
}

/// Каталог над `root` (включительно) с `.git` — корень репозитория есть.
fn in_git(root: &Path) -> bool {
    root.ancestors().any(|d| d.join(".git").exists())
}

/// `git -C root <args>`: stdout при коде 0; нет `git` или ошибка — `None`.
fn git(root: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let mut c = Command::new("git");
    c.arg("-C")
        .arg(root)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    let o = c.output().ok()?;
    o.status.success().then_some(o.stdout)
}

/// `git rev-parse HEAD` (если это git и `git` в `PATH`); ошибка — `None`.
pub fn git_head(root: &Path) -> Option<String> {
    if !in_git(root) {
        return None;
    }
    let out = git(root, &["rev-parse", "HEAD"])?;
    let s = String::from_utf8(out).ok()?.trim().to_ascii_lowercase();
    ((4..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())).then_some(s)
}

/// Пути, изменённые после `since` (`git diff --name-only <since>..HEAD`) и в рабочей копии
/// (`git status --porcelain`, оба имени переименования), относительно `root`.
pub fn git_changed_since(root: &Path, since: &str) -> Result<Vec<String>, CliError> {
    if since.starts_with('-') || since.is_empty() {
        return Err(CliError::input("--since: коммит или ссылка git"));
    }
    if !in_git(root) {
        return Err(CliError::input(format!(
            "--since: {} — не репозиторий git",
            root.display()
        )));
    }
    let fail = |what: &str| {
        CliError::config(format!(
            "git {what} не выполнен (git в PATH? коммит существует?)"
        ))
    };
    let prefix = git(root, &["rev-parse", "--show-prefix"]).ok_or_else(|| fail("rev-parse"))?;
    let prefix = String::from_utf8_lossy(&prefix).trim().to_string();
    let range = format!("{since}..HEAD");
    let diff =
        git(root, &["diff", "--name-only", "-z", &range, "--"]).ok_or_else(|| fail("diff"))?;
    let status = git(
        root,
        &["status", "--porcelain", "-z", "--untracked-files=all"],
    )
    .ok_or_else(|| fail("status"))?;
    let mut raw: Vec<String> = diff
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    raw.extend(porcelain_paths(&status));
    let mut out: Vec<String> = Vec::new();
    for p in raw {
        let Some(rel) = p.strip_prefix(&prefix) else {
            continue;
        };
        if let Ok(n) = normalize_ref_path(rel)
            && !out.iter().any(|o| same_path(o, &n))
        {
            out.push(n);
        }
    }
    Ok(out)
}

/// Пути `git status --porcelain -z`: `XY путь\0`, у переименования и копии — ещё старое имя.
pub fn porcelain_paths(z: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut it = z.split(|b| *b == 0).filter(|s| !s.is_empty());
    while let Some(rec) = it.next() {
        if rec.len() < 4 {
            continue;
        }
        let x = rec[0];
        out.push(String::from_utf8_lossy(&rec[3..]).into_owned());
        if matches!(x, b'R' | b'C')
            && let Some(old) = it.next()
        {
            out.push(String::from_utf8_lossy(old).into_owned());
        }
    }
    out
}

/// Корень проекта команд `zam` (как у хуков) и текущий каталог.
pub fn cli_root() -> (PathBuf, PathBuf) {
    let cwd = std::env::current_dir().unwrap_or_default();
    let root = hook::project_root(&cwd, &home_dir());
    (cwd, root)
}

/// Ссылка `{path, hash?, commit?}` записи журнала по файлу `--ref`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalRef {
    pub path: String,
    pub hash: Option<String>,
}

/// Пути `--ref` → ссылки (без повторов; ошибка пути — код 6). Файла нет — ссылка без хэша и
/// строка в stderr.
pub fn local_refs(files: &[String], cwd: &Path, root: &Path) -> Result<Vec<LocalRef>, CliError> {
    let mut out: Vec<LocalRef> = Vec::new();
    for f in files {
        let path =
            ref_path(f, cwd, root).map_err(|e| CliError::input(format!("--ref «{f}»: {e}")))?;
        if out.iter().any(|r| same_path(&r.path, &path)) {
            continue;
        }
        let hash = hash_rel(root, &path);
        if hash.is_none() {
            eprintln!("--ref {path}: файла нет в рабочей копии — ссылка без хэша");
        }
        out.push(LocalRef { path, hash });
    }
    if out.len() > REFS_MAX {
        return Err(CliError::input(format!(
            "--ref: не больше {REFS_MAX} ссылок"
        )));
    }
    Ok(out)
}

/// Поле запроса `refs: {files: [{path, hash?, commit?}]}` (коммит — один на все ссылки).
pub fn refs_field(refs: &[LocalRef], commit: Option<&str>) -> Value {
    let files: Vec<Value> = refs
        .iter()
        .map(|r| {
            let mut o = json!({ "path": r.path });
            if let Some(h) = &r.hash {
                o["hash"] = json!(h);
                if let Some(c) = commit {
                    o["commit"] = json!(c);
                }
            }
            o
        })
        .collect();
    json!({ "files": files })
}

/// Поле `refs` запроса записи журнала по путям `--ref` (корень — как у хуков); пусто — `None`.
pub fn request_refs(files: &[String]) -> Result<Option<Value>, CliError> {
    if files.is_empty() {
        return Ok(None);
    }
    let (cwd, root) = cli_root();
    let local = local_refs(files, &cwd, &root)?;
    let commit = local
        .iter()
        .any(|r| r.hash.is_some())
        .then(|| git_head(&root))
        .flatten();
    Ok(Some(refs_field(&local, commit.as_deref())))
}

/// `POST /files/verified {solution, files}` пачками до [`FILES_MAX`]; число подтверждённых.
pub fn post_verified(ctx: &Ctx, guid: &str, refs: &[LocalRef]) -> Result<i64, CliError> {
    let mut n = 0;
    for chunk in refs.chunks(FILES_MAX) {
        let files: Vec<Value> = chunk
            .iter()
            .map(|r| match &r.hash {
                Some(h) => json!({"path": r.path, "hash": h}),
                None => json!({"path": r.path}),
            })
            .collect();
        let v = read_json(ctx.client.post_json(
            "/files/verified",
            &json!({"solution": guid, "files": files}),
        )?)?;
        n += v["verified"].as_i64().unwrap_or(0);
    }
    Ok(n)
}

/// `POST /files/changed {solution, files}` пачками до [`FILES_MAX`]; число `stale`.
pub fn post_changed(
    client: &crate::client::Client,
    guid: &str,
    files: &[(String, String)],
) -> Result<i64, CliError> {
    let mut n = 0;
    for chunk in files.chunks(FILES_MAX) {
        let files: Vec<Value> = chunk
            .iter()
            .map(|(p, h)| json!({"path": p, "hash": h}))
            .collect();
        let v = read_json(
            client.post_json("/files/changed", &json!({"solution": guid, "files": files}))?,
        )?;
        n += v["stale"].as_i64().unwrap_or(0);
    }
    Ok(n)
}

/// `zam refs check [--since C]`.
pub fn check(ctx: &Ctx, since: Option<&str>, solution: Option<&str>) -> Result<(), CliError> {
    let guid = solution_guid(ctx, solution)?;
    let (_, root) = cli_root();
    let idx = read_json(
        ctx.client
            .get("/file-refs", &[("solution", guid.as_str())])?,
    )?;
    let mut paths: Vec<(String, Option<String>)> = idx["refs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| {
            Some((
                r["path"].as_str()?.to_string(),
                r["hash"].as_str().map(str::to_string),
            ))
        })
        .collect();
    if let Some(c) = since {
        let changed = git_changed_since(&root, c)?;
        paths.retain(|(p, _)| changed.iter().any(|c| same_path(c, p)));
    }
    let mut send: Vec<(String, String)> = Vec::new();
    let mut changed: Vec<String> = Vec::new();
    for (p, known) in &paths {
        let now = hash_rel(&root, p).unwrap_or_else(|| MISSING_HASH.to_string());
        match known {
            Some(k) if *k == now => {}
            Some(_) => {
                changed.push(p.clone());
                send.push((p.clone(), now));
            }
            // ориентир ссылки без хэша (правило I2 задачи 7)
            None => send.push((p.clone(), now)),
        }
    }
    let stale = if send.is_empty() {
        0
    } else {
        post_changed(&ctx.client, &guid, &send)?
    };
    if ctx.json {
        return emit_json(&json!({
            "checked": paths.len(),
            "changed": changed.len(),
            "stale": stale,
            "paths": changed,
        }));
    }
    let mut s = format!("проверено {}, изменилось {}", paths.len(), changed.len());
    if stale > 0 {
        s.push_str(&format!(" (устаревших ссылок: {stale})"));
    }
    s.push('\n');
    for p in &changed {
        s.push_str(&format!("  {p}\n"));
    }
    emit(s.as_bytes())
}

/// `zam refs verify <path>…`.
pub fn verify(ctx: &Ctx, files: &[String], solution: Option<&str>) -> Result<(), CliError> {
    let guid = solution_guid(ctx, solution)?;
    let (cwd, root) = cli_root();
    let mut refs: Vec<LocalRef> = Vec::new();
    for f in files {
        let path = ref_path(f, &cwd, &root).map_err(|e| CliError::input(format!("«{f}»: {e}")))?;
        if !refs.iter().any(|r| same_path(&r.path, &path)) {
            let hash = hash_rel(&root, &path);
            refs.push(LocalRef { path, hash });
        }
    }
    let n = post_verified(ctx, &guid, &refs)?;
    if ctx.json {
        return emit_json(&json!({ "verified": n }));
    }
    emit(format!("подтверждено ссылок: {n}\n").as_bytes())
}

/// Добавляет пути в список `refs:` frontmatter заметки (без повторов): нет frontmatter — он
/// создаётся; `refs:` блочным списком — пункты дописываются; `refs: [a, b]` или скаляр —
/// строка заменяется блочным списком. Перевод строки — как в файле. Многострочный
/// `refs: [` — ошибка (дополните вручную).
pub fn add_frontmatter_refs(content: &str, paths: &[String]) -> Result<String, CliError> {
    if paths.is_empty() {
        return Ok(content.to_string());
    }
    let (bom, text) = match content.strip_prefix('\u{feff}') {
        Some(t) => ("\u{feff}", t),
        None => ("", content),
    };
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let item = |p: &str| format!("  - {}", serde_json::to_string(p).unwrap_or_default());
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let is_delim = |l: &str| l.trim_end_matches(['\r', '\n']) == "---";
    let close = if lines.first().is_some_and(|l| is_delim(l)) {
        lines
            .iter()
            .skip(1)
            .position(|l| is_delim(l))
            .map(|i| i + 1)
    } else {
        None
    };
    let Some(close) = close else {
        let mut s = format!("{bom}---{nl}refs:{nl}");
        for p in paths {
            s.push_str(&item(p));
            s.push_str(nl);
        }
        s.push_str(&format!("---{nl}{nl}"));
        s.push_str(text);
        return Ok(s);
    };
    let key = (1..close).find(|&i| {
        let l = lines[i];
        !l.starts_with([' ', '\t', '#', '-'])
            && l.split_once(':').is_some_and(|(k, _)| k.trim() == "refs")
    });
    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    let Some(k) = key else {
        let mut add = format!("refs:{nl}");
        for p in paths {
            add.push_str(&item(p));
            add.push_str(nl);
        }
        out.insert(close, add);
        return Ok(format!("{bom}{}", out.concat()));
    };
    let value = lines[k].split_once(':').map(|(_, v)| v).unwrap_or("");
    let value = value.split(" #").next().unwrap_or("").trim();
    let mut have: Vec<String> = Vec::new();
    let mut last = k;
    if value.is_empty() {
        for (i, l) in lines.iter().enumerate().take(close).skip(k + 1) {
            let t = l.trim();
            if l.starts_with([' ', '\t', '-']) || t.is_empty() {
                if let Some(v) = t.strip_prefix('-') {
                    have.push(unquote(v.split(" #").next().unwrap_or("").trim()));
                    last = i;
                }
                continue;
            }
            break;
        }
        let mut add = String::new();
        for p in paths.iter().filter(|p| !contains_path(&have, p)) {
            add.push_str(&item(p));
            add.push_str(nl);
        }
        if !add.is_empty() {
            if !out[last].ends_with('\n') {
                out[last].push_str(nl);
            }
            out.insert(last + 1, add);
        }
        return Ok(format!("{bom}{}", out.concat()));
    }
    if value.starts_with('[') {
        let Some(inner) = value.strip_prefix('[').and_then(|v| v.strip_suffix(']')) else {
            return Err(CliError::input(
                "refs: многострочный список во frontmatter — дополните его вручную",
            ));
        };
        have.extend(
            inner
                .split(',')
                .map(|s| unquote(s.trim()))
                .filter(|s| !s.is_empty()),
        );
    } else if value != "~" && value != "null" {
        have.push(unquote(value));
    }
    let mut block = format!("refs:{nl}");
    for p in have
        .iter()
        .cloned()
        .chain(paths.iter().filter(|p| !contains_path(&have, p)).cloned())
    {
        block.push_str(&item(&p));
        block.push_str(nl);
    }
    out[k] = block;
    Ok(format!("{bom}{}", out.concat()))
}

fn contains_path(have: &[String], p: &str) -> bool {
    have.iter().any(|h| {
        normalize_ref_path(h)
            .map(|n| same_path(&n, p))
            .unwrap_or(false)
    })
}

/// Снимает кавычки YAML-скаляра (`"…"` — как JSON-строку, `'…'` — с `''`).
fn unquote(s: &str) -> String {
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        return serde_json::from_str::<String>(s).unwrap_or_else(|_| s[1..s.len() - 1].into());
    }
    if s.len() >= 2 && s.starts_with('\'') && s.ends_with('\'') {
        return s[1..s.len() - 1].replace("''", "'");
    }
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Правила копии совпадают с `zam_store::file_refs::normalize_ref_path` (принятие,
    /// итог и отказ), как `entry_ref_letters_match_store` у букв ссылок.
    #[test]
    fn ref_path_rules_match_store() {
        let long = "я".repeat(1001);
        let ok_long = "я".repeat(1000);
        for p in [
            "a\\b.rs",
            "./c/d.rs",
            "x/../e.rs",
            " src/a.rs ",
            "a//b",
            "a/./b/../c",
            "/abs",
            "../x",
            "a/../../x",
            "C:\\x",
            "c:/x",
            "c:x",
            "\\\\server\\share\\f",
            "./C:/x",
            "a/../C:/x",
            "a/../c:x",
            "a\\..\\D:\\x",
            "src/c:x",
            "a:b/c",
            "ab:c",
            "a\u{202E}b.rs",
            "a\u{200B}b.rs",
            "a\u{0}b",
            "a\nb",
            "",
            " ",
            ".",
            "./",
            "..",
            long.as_str(),
            ok_long.as_str(),
            "Src/Main.RS",
            "docs/b c.md",
        ] {
            let ours = normalize_ref_path(p).ok();
            let store = zam_store::file_refs::normalize_ref_path(p).ok();
            assert_eq!(ours, store, "{p:?}");
        }
        assert_eq!(PATH_MAX_CHARS, zam_store::file_refs::PATH_MAX_CHARS);
        assert_eq!(FILES_MAX, zam_store::file_refs::FILES_MAX);
        assert_eq!(REFS_MAX, zam_store::file_refs::REFS_MAX);
    }

    #[test]
    fn windows_rules() {
        let w = |p: &str| local_rules(p, true);
        assert_eq!(w("src\\Main.rs. ").as_deref(), Ok("src/Main.rs"));
        assert_eq!(w("a. /b..").as_deref(), Ok("a/b"));
        assert_eq!(w("./a/../b").as_deref(), Ok("./a/../b"));
        for d in [
            "CON", "nul", "aux.txt", "src/COM1", "lpt9.log", "con .x", "COM¹",
        ] {
            assert_eq!(w(d), Err(PathError::Device), "{d}");
        }
        for ok in ["CONSOLE.md", "com10", "nullable.rs", "lpt.rs"] {
            assert!(w(ok).is_ok(), "{ok}");
        }
        assert_eq!(w("ab:c"), Err(PathError::Stream));
        // `a:b` — путь от текущего каталога диска A: (абсолютный — ответ normalize_ref_path)
        assert_eq!(w("a:b").as_deref(), Ok("a:b"));
        assert_eq!(normalize_ref_path("a:b"), Err(PathError::Absolute));
        assert_eq!(w("src/file.txt:secret"), Err(PathError::Stream));
        // буква диска в начале — дело normalize_ref_path (абсолютный путь)
        assert_eq!(w("C:/x").as_deref(), Ok("C:/x"));
        assert_eq!(w("a/ ."), Err(PathError::Empty));
        // на других ОС правила не действуют
        assert_eq!(local_rules("CON. ", false).as_deref(), Ok("CON. "));
    }

    #[test]
    fn porcelain_records() {
        let z = b" M src/a.rs\0R  new.rs\0old.rs\0?? docs/x y.md\0A  b.rs\0";
        assert_eq!(
            porcelain_paths(z),
            ["src/a.rs", "new.rs", "old.rs", "docs/x y.md", "b.rs"]
        );
    }

    fn fm_refs(s: &str) -> Vec<String> {
        // тот же разбор, что у сервера
        let doc = zam_markdown::parse(s);
        doc.frontmatter.map(|f| f.refs).unwrap_or_default()
    }

    #[test]
    fn frontmatter_refs_merge() {
        let p = vec!["src/a.rs".to_string(), "docs/b c.md".to_string()];
        let s = add_frontmatter_refs("# Заголовок\n", &p).unwrap();
        assert!(s.starts_with("---\nrefs:\n"), "{s}");
        assert_eq!(fm_refs(&s), p);
        assert!(s.ends_with("# Заголовок\n"));
        let s = add_frontmatter_refs("---\ntags: [x]\n---\n\nтекст\n", &p).unwrap();
        assert_eq!(fm_refs(&s), p);
        assert!(s.contains("tags: [x]"));
        let s = add_frontmatter_refs("---\nrefs:\n  - src/a.rs\ntags: [x]\n---\nт\n", &p).unwrap();
        assert_eq!(fm_refs(&s), ["src/a.rs", "docs/b c.md"]);
        let s = add_frontmatter_refs("---\nrefs: [old.rs, \"src/a.rs\"]\n---\nт\n", &p).unwrap();
        assert_eq!(fm_refs(&s), ["old.rs", "src/a.rs", "docs/b c.md"]);
        let s = add_frontmatter_refs("---\r\nrefs: one.rs\r\n---\r\nт\r\n", &p).unwrap();
        assert_eq!(fm_refs(&s), ["one.rs", "src/a.rs", "docs/b c.md"]);
        assert!(
            !s.replace("\r\n", "").contains('\n'),
            "CRLF сохранён: {s:?}"
        );
        let s = add_frontmatter_refs("\u{feff}---\nl0: x\n---\nт\n", &p).unwrap();
        assert!(s.starts_with('\u{feff}'));
        assert!(add_frontmatter_refs("---\nrefs: [a,\n  b]\n---\n", &p).is_err());
    }
}
