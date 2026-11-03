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
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::UNIX_EPOCH;

use globset::GlobSet;

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

/// Отбор по языку и маскам.
struct Filter<'a> {
    cfg: &'a CodeConfig,
    exclude: GlobSet,
}

impl Filter<'_> {
    fn lang_ok(&self, rel: &str) -> bool {
        zam_code::lang_of(rel).is_some_and(|l| self.cfg.lang_on(l))
    }

    fn excluded(&self, rel: &str) -> bool {
        rel.starts_with(".zeroaimemory/") || self.exclude.is_match(rel)
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
    let f = Filter {
        cfg,
        exclude: cfg.exclude_set(),
    };
    let mut out = match from_git(root, &f)? {
        Some(v) => v,
        None => walk(root, &f, cfg.max_files as usize + 1),
    };
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out.dedup_by(|a, b| a.path == b.path);
    if out.len() > cfg.max_files as usize {
        return Err(CliError::config(format!(
            "файлов кода {} — больше code.max_files = {}: сузьте code.exclude или code.languages",
            out.len(),
            cfg.max_files
        )));
    }
    Ok(out)
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

/// Отпечатки по git: не репозиторий (или корень в игнорируемой части) — `None`; сбой `git` в
/// настоящем репозитории (`safe.directory`, битый индекс, нет `git`) — ошибка конфигурации, а
/// не тихий обход: обход дал бы другой набор файлов, и сервер стёр бы часть карты.
fn from_git(root: &Path, f: &Filter) -> Result<Option<Vec<Fingerprint>>, CliError> {
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
    let mut case = CaseMap::new(root);
    let mut out = Vec::with_capacity(cands.len());
    for (raw, oid) in cands {
        let Ok(rel) = refs::normalize_ref_path(&raw) else {
            continue;
        };
        if !f.lang_ok(&rel) || f.excluded(&rel) {
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
/// (ревью задачи 5, M9).
fn walk(root: &Path, f: &Filter, limit: usize) -> Vec<Fingerprint> {
    let mut out = Vec::new();
    let mut stack: Vec<(PathBuf, String)> = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        if out.len() >= limit {
            break;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
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
}
