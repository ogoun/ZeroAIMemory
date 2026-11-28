//! Отпечатки файлов кода (спецификация M5.6 §4; решение составителя плана 6).
//!
//! - **В git:** список и oid — `git ls-files -s -v -z` (пути относительно корня решения — git
//!   сам сужает список подкаталогом; oid — только без `skip-worktree` и `assume-unchanged`),
//!   изменённые и неотслеживаемые (не игнорируемые) —
//!   `git status --porcelain=v2 -z --untracked-files=all -- .` (пути от корня репозитория —
//!   срезается `--show-prefix`). Чистый файл — отпечаток `oid:<sha>` и `git_oid`, изменённый
//!   или новый — `mt:<размер>:<mtime_ns>` без `git_oid`. Подмодули и символические ссылки
//!   индекса пропускаются; удалённый в рабочей копии файл в список не входит. Корень внутри
//!   игнорируемой части чужого репозитория (пустой индекс и `check-ignore .`) — как вне git.
//!   Сбой `git` в настоящем репозитории (`safe.directory`, битый индекс, нет `git`) — ошибка, а
//!   не обход (ревью задачи 5, I3). `git` — с `GIT_OPTIONAL_LOCKS=0` ([`git_run`]). Windows:
//!   файл за точкой соединения или символической ссылкой каталога — мимо.
//! - **Вне git:** обход каталогов стеком (без рекурсии), без `.git`, `.zeroaimemory`, `target`,
//!   `node_modules`, `dist`, `build` и скрытых каталогов; символические ссылки (и точки
//!   соединения Windows) не обходятся и не читаются; отпечаток `mt:…`.
//! - **Отбор:** язык по расширению ([`zam_code::lang_of`]) и `code.languages`, маски
//!   `code.exclude` (на Windows — без учёта регистра); путь — по правилам ссылок
//!   ([`refs::normalize_ref_path`]), `/`, регистр — из листинга каталогов (один `read_dir` на
//!   каталог, Review Focus 5); повтор пути (другой регистр в индексе) — один раз. Больше
//!   `code.max_files` файлов — ошибка конфигурации (обход вне git останавливается на
//!   `max_files + 1`).
//! - **Вендорный и созданный код** (M7.5 §8.6 п. 4, [`GitAttrs`]): файлы с атрибутами
//!   `linguist-vendored` или `linguist-generated` из `.gitattributes` корня и подкаталогов в карту
//!   не входят (`code.exclude_gitattributes = false` — входят); их число — [`Scan::gitattributes`].
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::UNIX_EPOCH;

use globset::{GlobBuilder, GlobMatcher, GlobSet};

use super::config::CodeConfig;
use crate::client::CliError;
use crate::cmd::refs;

/// Отпечаток файла.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    /// Относительно корня, `/`, регистр ФС.
    pub path: String,
    /// `oid:<sha>` | `mt:<размер>:<mtime_ns>`.
    pub fp: String,
    pub size: u64,
    /// oid чистого файла git.
    pub git_oid: Option<String>,
}

/// Каталоги, которые обход вне git пропускает (и скрытые — с `.`).
const SKIP_DIRS: [&str; 6] = [
    ".git",
    ".zeroaimemory",
    "target",
    "node_modules",
    "dist",
    "build",
];

/// Итог отбора файлов кода.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scan {
    /// Отпечатки файлов карты, по пути.
    pub files: Vec<Fingerprint>,
    /// Файлы кода (язык включён, масок `exclude` нет), исключённые по `.gitattributes`, по пути.
    pub gitattributes: Vec<String>,
}

/// Имя файла атрибутов git.
const GITATTRIBUTES: &str = ".gitattributes";
/// Предел файла `.gitattributes`, байт: больше — не читается.
const GITATTRIBUTES_MAX: u64 = 1024 * 1024;

/// Правило `.gitattributes` об атрибутах linguist.
struct AttrRule {
    /// Каталог файла правила от корня (`""` или `a/b/`), на Windows — в нижнем регистре.
    dir: String,
    glob: GlobMatcher,
    /// `Some(true)` — установлен, `Some(false)` — снят (`-attr`, `!attr`, `attr=false`), `None`
    /// — не упомянут.
    vendored: Option<bool>,
    generated: Option<bool>,
}

/// Атрибуты `linguist-vendored` и `linguist-generated` из `.gitattributes` (M7.5 §8.6 п. 4) —
/// по правилам git: маска без `/` — по имени где угодно ниже каталога файла, с `/` — от его
/// каталога (`/` в начале — тоже), маска каталога (`/` в конце) файлов не задевает; позднее
/// правило сильнее раннего, файл глубже — сильнее файла выше. Файл исключается, если в итоге хотя
/// бы один из двух атрибутов установлен (`attr`, `attr=…` кроме `=false`). Макросы (`[attr]`) не
/// раскрываются; на Windows — без учёта регистра, как маски `exclude`.
#[derive(Default)]
pub(super) struct GitAttrs {
    /// Правила по старшинству: каталоги от корня вглубь, строки файла по порядку.
    rules: Vec<AttrRule>,
}

/// Путь для сравнения с каталогом правила: на Windows — в нижнем регистре.
fn path_key(p: &str) -> String {
    if cfg!(windows) {
        p.to_lowercase()
    } else {
        p.to_string()
    }
}

/// `attr`, `-attr`, `!attr`, `attr=value` → (имя, установлен): `-`, `!` и `=false` — снят.
fn parse_attr(a: &str) -> (&str, bool) {
    if let Some(n) = a.strip_prefix('-').or_else(|| a.strip_prefix('!')) {
        return (n, false);
    }
    match a.split_once('=') {
        Some((n, v)) => (n, v != "false"),
        None => (a, true),
    }
}

/// Маска правила файла каталога `dir` как glob от корня; маска каталога — `None`.
fn attr_glob(dir: &str, pat: &str) -> Option<GlobMatcher> {
    let pat = pat
        .strip_prefix('"')
        .and_then(|p| p.strip_suffix('"'))
        .unwrap_or(pat);
    if pat.is_empty() || pat.ends_with('/') {
        return None;
    }
    let base = globset::escape(dir);
    let full = match pat.strip_prefix('/') {
        Some(p) => format!("{base}{p}"),
        None if pat.contains('/') => format!("{base}{pat}"),
        None => format!("{base}**/{pat}"),
    };
    GlobBuilder::new(&full)
        .literal_separator(true)
        .case_insensitive(cfg!(windows))
        .build()
        .ok()
        .map(|g| g.compile_matcher())
}

impl GitAttrs {
    /// Правила текста `.gitattributes` каталога `dir` (`""` или `a/b/`): только строки с
    /// атрибутами linguist; комментарии, макросы и неразборные маски пропускаются.
    pub(super) fn add_file(&mut self, dir: &str, text: &str) {
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with("[attr]") {
                continue;
            }
            let mut parts = line.split_whitespace();
            let Some(pat) = parts.next() else {
                continue;
            };
            let (mut vendored, mut generated) = (None, None);
            for a in parts {
                match parse_attr(a) {
                    ("linguist-vendored", set) => vendored = Some(set),
                    ("linguist-generated", set) => generated = Some(set),
                    _ => {}
                }
            }
            if vendored.is_none() && generated.is_none() {
                continue;
            }
            if let Some(glob) = attr_glob(dir, pat) {
                self.rules.push(AttrRule {
                    dir: path_key(dir),
                    glob,
                    vendored,
                    generated,
                });
            }
        }
    }

    /// Файл `.gitattributes` каталога `dir` под `root`, если он есть.
    fn add_dir(&mut self, root: &Path, dir: &str) {
        let path = root.join(dir).join(GITATTRIBUTES);
        let Ok(m) = std::fs::metadata(&path) else {
            return;
        };
        if !m.is_file() || m.len() > GITATTRIBUTES_MAX {
            return;
        }
        if let Ok(text) = std::fs::read_to_string(&path) {
            self.add_file(dir, &text);
        }
    }

    /// Путь `rel` исключён: итог правил его каталогов — `linguist-vendored` или
    /// `linguist-generated` установлен.
    pub(super) fn excluded(&self, rel: &str) -> bool {
        if self.rules.is_empty() {
            return false;
        }
        let key = path_key(rel);
        let (mut vendored, mut generated) = (false, false);
        for r in self
            .rules
            .iter()
            .filter(|r| key.starts_with(&r.dir) && r.glob.is_match(rel))
        {
            vendored = r.vendored.unwrap_or(vendored);
            generated = r.generated.unwrap_or(generated);
        }
        vendored || generated
    }
}

/// Отбор по языку, маскам и `.gitattributes`.
struct Filter<'a> {
    cfg: &'a CodeConfig,
    exclude: GlobSet,
    attrs: GitAttrs,
}

impl Filter<'_> {
    fn lang_ok(&self, rel: &str) -> bool {
        zam_code::lang_of(rel).is_some_and(|l| self.cfg.lang_on(l))
    }

    fn excluded(&self, rel: &str) -> bool {
        rel.starts_with(".zeroaimemory/") || self.exclude.is_match(rel)
    }

    /// Исключён по `.gitattributes` (при `code.exclude_gitattributes`).
    fn by_attrs(&self, rel: &str) -> bool {
        self.cfg.exclude_gitattributes && self.attrs.excluded(rel)
    }
}

fn mtime_ns(m: &std::fs::Metadata) -> u128 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos())
}

fn mt_fp(m: &std::fs::Metadata) -> String {
    format!("mt:{}:{}", m.len(), mtime_ns(m))
}

/// Отпечатки кода под `root`, по пути.
pub fn fingerprints(root: &Path, cfg: &CodeConfig) -> Result<Vec<Fingerprint>, CliError> {
    Ok(scan(root, cfg)?.files)
}

/// Отбор файлов кода под `root`: отпечатки и исключённые по `.gitattributes`.
pub fn scan(root: &Path, cfg: &CodeConfig) -> Result<Scan, CliError> {
    let mut f = Filter {
        cfg,
        exclude: cfg.exclude_set(),
        attrs: GitAttrs::default(),
    };
    let mut by_attrs = Vec::new();
    let (mut out, walked) = match from_git(root, &mut f, &mut by_attrs)? {
        Some(v) => (v, false),
        None => (
            walk(root, &mut f, cfg.max_files as usize + 1, &mut by_attrs),
            true,
        ),
    };
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out.dedup_by(|a, b| a.path == b.path);
    by_attrs.sort();
    by_attrs.dedup();
    if out.len() > cfg.max_files as usize {
        // обход вне git оборван на `max_files + 1`: точного числа нет (ревью задачи 9A, M3)
        let n = if walked {
            String::new()
        } else {
            format!("{} — ", out.len())
        };
        return Err(CliError::config(format!(
            "файлов кода {n}больше code.max_files = {}: сузьте code.exclude или code.languages",
            cfg.max_files
        )));
    }
    Ok(Scan {
        files: out,
        gitattributes: by_attrs,
    })
}

/// Записи `git ls-files -s -v -z`: `<тег> <mode> <oid> <stage>\t<путь>` → путь → oid (без
/// подмодулей и символических ссылок; конфликт — несколько стадий, oid нет). oid — только у тега
/// `H`: у `skip-worktree` (`S`) и `assume-unchanged` (строчные) `git status` правку содержимого
/// не видит, и отпечаток `oid:` отдал бы старый хэш (ревью задачи 5, M9).
fn ls_files(z: &[u8]) -> Vec<(String, Option<String>)> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut out: Vec<(String, Option<String>)> = Vec::new();
    for rec in z.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let Some(tab) = rec.iter().position(|b| *b == b'\t') else {
            continue;
        };
        let (meta, path) = (String::from_utf8_lossy(&rec[..tab]), &rec[tab + 1..]);
        let Ok(path) = std::str::from_utf8(path) else {
            continue;
        };
        let mut it = meta.split(' ');
        let (Some(tag), Some(mode), Some(oid), Some(stage)) =
            (it.next(), it.next(), it.next(), it.next())
        else {
            continue;
        };
        if mode == "160000" || mode == "120000" {
            continue;
        }
        let oid = (stage == "0" && tag == "H").then(|| oid.to_ascii_lowercase());
        match seen.get(path) {
            Some(&i) => out[i].1 = None,
            None => {
                seen.insert(path.to_string(), out.len());
                out.push((path.to_string(), oid));
            }
        }
    }
    out
}

/// `git status --porcelain=v2 -z`: изменённые (`1`, `2`, `u`) и неотслеживаемые (`?`) пути от
/// корня репозитория. У записи `2` (переименование) следом идёт старое имя — пропускается.
fn porcelain_v2(z: &[u8]) -> (HashSet<String>, Vec<String>) {
    let (mut dirty, mut untracked) = (HashSet::new(), Vec::new());
    let mut it = z.split(|b| *b == 0).filter(|r| !r.is_empty());
    while let Some(rec) = it.next() {
        let text = String::from_utf8_lossy(rec);
        let fields = |n: usize| text.splitn(n, ' ').nth(n - 1).map(str::to_string);
        match rec[0] {
            b'1' => dirty.extend(fields(9)),
            b'2' => {
                dirty.extend(fields(10));
                it.next();
            }
            b'u' => dirty.extend(fields(11)),
            b'?' => untracked.extend(fields(2)),
            _ => {}
        }
    }
    (dirty, untracked)
}

/// Репозиторий над `root`: `Some(true)` — настоящий (`.git` — каталог или файл worktree с
/// существующим `gitdir`), `Some(false)` — маркер без репозитория, `None` — git нет.
fn repo_marker(root: &Path) -> Option<bool> {
    let dir = root.ancestors().find(|d| d.join(".git").exists())?;
    let g = dir.join(".git");
    if g.is_dir() {
        return Some(true);
    }
    let text = std::fs::read_to_string(&g).unwrap_or_default();
    let gitdir = text
        .lines()
        .find_map(|l| l.strip_prefix("gitdir:"))
        .map(str::trim)
        .filter(|x| !x.is_empty());
    Some(gitdir.is_some_and(|x| dir.join(x).is_dir()))
}

/// `git -C root <args>` без необязательных замков (`GIT_OPTIONAL_LOCKS=0`: `status` не берёт
/// `index.lock` параллельно с git пользователя); ошибка — текст stderr.
pub fn git_run(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let o = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("git не запустился: {e}"))?;
    if o.status.success() {
        Ok(o.stdout)
    } else {
        let err = String::from_utf8_lossy(&o.stderr);
        Err(err.trim().chars().take(300).collect())
    }
}

/// Регистр путей из листинга каталогов (ревью задачи 5, M3): один `read_dir` на каталог вместо
/// `canonicalize` на каждый файл. Windows: путь `rel` в регистре ФС; компонент — точка
/// соединения или символическая ссылка каталога (куда бы она ни вела) — `None`. Не Windows — `rel`
/// (git не отдаёт пути за символической ссылкой каталога).
struct CaseMap<'a> {
    root: &'a Path,
    /// Каталог (относительно корня, регистр ФС) → его листинг; не читается — `None`.
    dirs: HashMap<String, Option<Listing>>,
}

/// Листинг каталога: имя в нижнем регистре → `(имя ФС, ссылка)` (несколько — каталог с учётом
/// регистра).
type Listing = HashMap<String, Vec<(String, bool)>>;

impl<'a> CaseMap<'a> {
    fn new(root: &'a Path) -> Self {
        CaseMap {
            root,
            dirs: HashMap::new(),
        }
    }

    /// Имя `name` в каталоге `dir` в регистре ФС и признак ссылки; точное имя сильнее
    /// совпадения без регистра (каталоги с учётом регистра).
    fn entry(&mut self, dir: &str, name: &str) -> Option<(String, bool)> {
        let root = self.root;
        let listing = self.dirs.entry(dir.to_string()).or_insert_with(|| {
            let rd = std::fs::read_dir(root.join(dir)).ok()?;
            let mut m = Listing::new();
            for e in rd.flatten() {
                let (Ok(t), Some(n)) = (e.file_type(), e.file_name().to_str().map(str::to_string))
                else {
                    continue;
                };
                m.entry(n.to_lowercase())
                    .or_default()
                    .push((n, t.is_symlink()));
            }
            Some(m)
        });
        let all = listing.as_ref()?.get(&name.to_lowercase())?;
        all.iter()
            .find(|(n, _)| n == name)
            .or_else(|| all.first())
            .cloned()
    }

    fn path(&mut self, rel: &str) -> Option<String> {
        if !cfg!(windows) {
            return Some(rel.to_string());
        }
        let mut real = String::with_capacity(rel.len());
        for part in rel.split('/') {
            let (name, link) = self.entry(&real, part)?;
            if link {
                return None;
            }
            if !real.is_empty() {
                real.push('/');
            }
            real.push_str(&name);
        }
        Some(real)
    }
}

/// `.gitattributes` из путей листинга git (отслеживаемые и неотслеживаемые) и корня — от корня
/// вглубь (старшинство правил).
fn git_attrs(root: &Path, cands: &[(String, Option<String>)]) -> GitAttrs {
    let mut dirs: Vec<String> = cands
        .iter()
        .filter_map(|(raw, _)| refs::normalize_ref_path(raw).ok())
        .filter_map(|p| {
            let dir = p.strip_suffix(GITATTRIBUTES)?;
            (dir.is_empty() || dir.ends_with('/')).then(|| dir.to_string())
        })
        .collect();
    dirs.push(String::new());
    dirs.sort_by(|a, b| (a.matches('/').count(), a).cmp(&(b.matches('/').count(), b)));
    dirs.dedup();
    let mut attrs = GitAttrs::default();
    for d in &dirs {
        attrs.add_dir(root, d);
    }
    attrs
}

/// Отпечатки по git: не репозиторий (или корень в игнорируемой части) — `None`; сбой `git` в
/// настоящем репозитории (`safe.directory`, битый индекс, нет `git`) — ошибка конфигурации, а
/// не тихий обход: обход дал бы другой набор файлов, и сервер стёр бы часть карты. Файлы,
/// исключённые по `.gitattributes`, — в `by_attrs`.
fn from_git(
    root: &Path,
    f: &mut Filter,
    by_attrs: &mut Vec<String>,
) -> Result<Option<Vec<Fingerprint>>, CliError> {
    match repo_marker(root) {
        None | Some(false) => return Ok(None),
        Some(true) => {}
    }
    let fail = |what: &str, e: String| {
        CliError::config(format!(
            "git {what} в {} не выполнен: {e} (git в PATH? safe.directory?)",
            root.display()
        ))
    };
    let prefix =
        git_run(root, &["rev-parse", "--show-prefix"]).map_err(|e| fail("rev-parse", e))?;
    let prefix = String::from_utf8_lossy(&prefix).trim().to_string();
    let ls =
        ls_files(&git_run(root, &["ls-files", "-s", "-v", "-z"]).map_err(|e| fail("ls-files", e))?);
    if ls.is_empty() && git_run(root, &["check-ignore", "-q", "."]).is_ok() {
        return Ok(None);
    }
    let st = git_run(
        root,
        &[
            "status",
            "--porcelain=v2",
            "-z",
            "--untracked-files=all",
            "--",
            ".",
        ],
    )
    .map_err(|e| fail("status", e))?;
    let (dirty_repo, untracked_repo) = porcelain_v2(&st);
    let strip = |p: &String| p.strip_prefix(prefix.as_str()).map(str::to_string);
    let dirty: HashSet<String> = dirty_repo.iter().filter_map(strip).collect();
    let mut cands: Vec<(String, Option<String>)> = ls
        .into_iter()
        .map(|(p, oid)| {
            let oid = oid.filter(|_| !dirty.contains(&p));
            (p, oid)
        })
        .collect();
    cands.extend(untracked_repo.iter().filter_map(strip).map(|p| (p, None)));
    if f.cfg.exclude_gitattributes {
        f.attrs = git_attrs(root, &cands);
    }
    let mut case = CaseMap::new(root);
    let mut out = Vec::with_capacity(cands.len());
    for (raw, oid) in cands {
        let Ok(rel) = refs::normalize_ref_path(&raw) else {
            continue;
        };
        if !f.lang_ok(&rel) || f.excluded(&rel) {
            continue;
        }
        if f.by_attrs(&rel) {
            by_attrs.push(rel);
            continue;
        }
        let Ok(m) = std::fs::symlink_metadata(root.join(&rel)) else {
            continue; // удалён в рабочей копии
        };
        if !m.is_file() {
            continue;
        }
        let Some(path) = case.path(&rel) else {
            continue; // через точку соединения или символическую ссылку каталога
        };
        if path != rel && f.excluded(&path) {
            continue;
        }
        if path != rel && f.by_attrs(&path) {
            by_attrs.push(path);
            continue;
        }
        let (fp, git_oid) = match oid {
            Some(o) => (format!("oid:{o}"), Some(o)),
            None => (mt_fp(&m), None),
        };
        out.push(Fingerprint {
            path,
            fp,
            size: m.len(),
            git_oid,
        });
    }
    Ok(Some(out))
}

/// Обход вне git (стек каталогов, без символических ссылок и точек соединения) до `limit`
/// файлов: больше `code.max_files` — всё равно ошибка, полный обход огромного дерева не нужен
/// (ревью задачи 5, M9). `.gitattributes` каталога читается до его файлов: каталоги-предки
/// обходятся раньше потомков, так что правила копятся от корня вглубь (старшинство). Файлы,
/// исключённые по `.gitattributes`, — в `by_attrs`.
fn walk(root: &Path, f: &mut Filter, limit: usize, by_attrs: &mut Vec<String>) -> Vec<Fingerprint> {
    let mut out = Vec::new();
    let mut stack: Vec<(PathBuf, String)> = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        if out.len() >= limit {
            break;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        if f.cfg.exclude_gitattributes {
            f.attrs.add_dir(root, &prefix);
        }
        for e in rd.flatten() {
            let Ok(t) = e.file_type() else {
                continue;
            };
            let Some(name) = e.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if t.is_symlink() {
                continue;
            }
            let rel = format!("{prefix}{name}");
            if t.is_dir() {
                if !name.starts_with('.') && !SKIP_DIRS.contains(&name.as_str()) {
                    stack.push((e.path(), format!("{rel}/")));
                }
                continue;
            }
            if !t.is_file() || !f.lang_ok(&rel) || f.excluded(&rel) {
                continue;
            }
            if refs::normalize_ref_path(&rel).as_deref() != Ok(rel.as_str()) {
                continue;
            }
            if f.by_attrs(&rel) {
                by_attrs.push(rel);
                continue;
            }
            let Ok(m) = e.metadata() else {
                continue;
            };
            out.push(Fingerprint {
                path: rel,
                fp: mt_fp(&m),
                size: m.len(),
                git_oid: None,
            });
            if out.len() >= limit {
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Записи через NUL (как `-z`).
    fn z(recs: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        for r in recs {
            out.extend_from_slice(r.as_bytes());
            out.push(0);
        }
        out
    }

    #[test]
    fn ls_files_records() {
        let z = z(&[
            "H 100644 aaaa 0\tsrc/a.rs",
            "H 160000 bbbb 0\tsub",
            "H 120000 cccc 0\tlink.rs",
            "M 100644 d1 1\tc.rs",
            "M 100644 d2 2\tc.rs",
            "H 100755 EEEE 0\tbin.py",
            "S 100644 ffff 0\tsparse.rs",
            "h 100644 9999 0\tassumed.rs",
        ]);
        assert_eq!(
            ls_files(&z),
            [
                ("src/a.rs".to_string(), Some("aaaa".to_string())),
                ("c.rs".to_string(), None),
                ("bin.py".to_string(), Some("eeee".to_string())),
                ("sparse.rs".to_string(), None),
                ("assumed.rs".to_string(), None),
            ]
        );
    }

    #[test]
    fn porcelain_v2_records() {
        let z = z(&[
            "1 .M N... 100644 100644 100644 h1 h2 src/a b.rs",
            "2 R. N... 100644 100644 100644 h1 h2 R100 new.rs",
            "old.rs",
            "u UU N... 100644 100644 100644 100644 h1 h2 h3 conf.rs",
            "? fresh dir/x.py",
            "! ign.rs",
        ]);
        let (dirty, untracked) = porcelain_v2(&z);
        let mut d: Vec<_> = dirty.into_iter().collect();
        d.sort();
        assert_eq!(d, ["conf.rs", "new.rs", "src/a b.rs"]);
        assert_eq!(untracked, ["fresh dir/x.py"]);
    }

    /// Каталог проекта теста под `target/tmp` (в git-игнорируемой части репозитория — обход).
    fn proj(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/tmp/zam-cli-code-scan")
            .join(name);
        let _ = std::fs::remove_dir_all(&root);
        for (rel, body) in files {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        root
    }

    /// Проект `files` с `.gitattributes` — обходом вне git и (если git есть) по git: файлы карты
    /// и исключённые по `.gitattributes`.
    fn scan_both(
        name: &str,
        files: &[(&str, &str)],
        cfg: &CodeConfig,
    ) -> Vec<(Scan, &'static str)> {
        let mut out = Vec::new();
        let root = proj(name, files);
        out.push((scan(&root, cfg).unwrap(), "обход"));
        let git = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&root)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output();
        if git.is_ok_and(|o| o.status.success()) {
            out.push((scan(&root, cfg).unwrap(), "git"));
        }
        out
    }

    fn paths(s: &Scan) -> Vec<&str> {
        s.files.iter().map(|f| f.path.as_str()).collect()
    }

    /// M7.5 §8.6 п. 4: `linguist-vendored` и `linguist-generated` (в том числе `=true`) из
    /// `.gitattributes` корня и подкаталогов — не в карте, их число — в итоге; маска без `/` — по
    /// имени где угодно ниже каталога файла, с `/` — от его каталога, `*` не переходит `/`, маска
    /// каталога файлов не задевает.
    #[test]
    fn gitattributes_vendored_generated_excluded() {
        let mut a = GitAttrs::default();
        a.add_file(
            "",
            "# комментарий\n*.rs text eol=lf\nvendor/** linguist-vendored\n\
             *.pb.go linguist-generated=true\n/top.js linguist-vendored\nlibdir/ linguist-vendored\n\
             [attr]gen linguist-generated\n",
        );
        a.add_file("web/", "gen/*.ts linguist-generated\n");
        for (p, want) in [
            ("vendor/lib/x.rs", true),
            ("api/v1/msg.pb.go", true),
            ("msg.pb.go", true),
            ("top.js", true),
            ("src/top.js", false),
            ("web/gen/a.ts", true),
            ("gen/a.ts", false),
            ("web/gen/deep/a.ts", false),
            ("libdir/a.rs", false),
            ("src/main.rs", false),
        ] {
            assert_eq!(a.excluded(p), want, "{p}");
        }
        let files = [
            (
                ".gitattributes",
                "third_party/** linguist-vendored\n*.gen.go linguist-generated\n",
            ),
            ("src/main.rs", "fn main() {}\n"),
            ("src/api.gen.go", "package api\n"),
            ("third_party/lib/x.py", "def x():\n    pass\n"),
            ("web/.gitattributes", "*.ts linguist-generated\n"),
            ("web/client.ts", "export const a = 1;\n"),
            ("other/client.ts", "export const b = 2;\n"),
            ("README.md", "# не код\n"),
        ];
        let cfg = CodeConfig::default();
        let runs = scan_both("gitattr-excluded", &files, &cfg);
        for (s, mode) in &runs {
            assert_eq!(paths(s), ["other/client.ts", "src/main.rs"], "{mode}");
            assert_eq!(
                s.gitattributes,
                ["src/api.gen.go", "third_party/lib/x.py", "web/client.ts"],
                "{mode}"
            );
        }
        // `exclude_gitattributes = false` — всё в карте, исключённых нет
        let off = CodeConfig {
            exclude_gitattributes: false,
            ..CodeConfig::default()
        };
        for (s, mode) in scan_both("gitattr-off", &files, &off) {
            assert_eq!(s.files.len(), 5, "{mode}: {:?}", paths(&s));
            assert!(s.gitattributes.is_empty(), "{mode}");
        }
        // маски `exclude` сильнее: исключённый маской в число по `.gitattributes` не входит
        let masked = CodeConfig {
            exclude: vec!["third_party/**".into()],
            ..CodeConfig::default()
        };
        for (s, mode) in scan_both("gitattr-masked", &files, &masked) {
            assert_eq!(
                s.gitattributes,
                ["src/api.gen.go", "web/client.ts"],
                "{mode}"
            );
        }
    }

    /// Снятый атрибут (`-attr`, `attr=false`, `!attr`) файл оставляет; позднее правило сильнее
    /// раннего, правило файла глубже — сильнее правила корня.
    #[test]
    fn gitattributes_unset_keeps_file() {
        let mut a = GitAttrs::default();
        a.add_file("", "vendor/** linguist-vendored\n");
        a.add_file(
            "vendor/",
            "keep.rs -linguist-vendored\nown.rs linguist-vendored=false\nreset.rs !linguist-vendored\n",
        );
        for (p, want) in [
            ("vendor/keep.rs", false),
            ("vendor/own.rs", false),
            ("vendor/reset.rs", false),
            ("vendor/other.rs", true),
        ] {
            assert_eq!(a.excluded(p), want, "{p}");
        }
        let mut b = GitAttrs::default();
        b.add_file(
            "",
            "*.js linguist-generated\nlib.js -linguist-generated\nback.js -linguist-generated\n\
             back.js linguist-generated\n",
        );
        assert!(b.excluded("x.js") && !b.excluded("lib.js") && b.excluded("back.js"));
        // снят один атрибут — второй всё равно исключает
        let mut c = GitAttrs::default();
        c.add_file(
            "",
            "*.go linguist-vendored linguist-generated\nmain.go -linguist-vendored\n",
        );
        assert!(c.excluded("main.go"));
        let files = [
            (".gitattributes", "vendor/** linguist-vendored\n"),
            ("vendor/.gitattributes", "keep.rs -linguist-vendored\n"),
            ("vendor/keep.rs", "pub fn keep() {}\n"),
            ("vendor/drop.rs", "pub fn drop_me() {}\n"),
        ];
        for (s, mode) in scan_both("gitattr-unset", &files, &CodeConfig::default()) {
            assert_eq!(paths(&s), ["vendor/keep.rs"], "{mode}");
            assert_eq!(s.gitattributes, ["vendor/drop.rs"], "{mode}");
        }
    }
}
