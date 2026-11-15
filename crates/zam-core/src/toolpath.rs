//! Пути файлов из входа инструментов агента (хуки `pre-tool`/`post-tool`, адаптеры снимка
//! сессии §8а): путь относительно корня решения и файлы патча `apply_patch` (Codex). Только
//! строки и лексическая нормализация — файловая система не читается.
use std::path::{Path, PathBuf};

/// Пути файлов из текста патча `apply_patch` (Codex): строки `*** Add File: <путь>`,
/// `*** Update File: <путь>`, `*** Delete File: <путь>`, `*** Move to: <путь>`.
pub fn patch_files(patch: &str) -> Vec<String> {
    const HEADS: [&str; 4] = [
        "*** Add File:",
        "*** Update File:",
        "*** Delete File:",
        "*** Move to:",
    ];
    let mut out: Vec<String> = Vec::new();
    for line in patch.lines() {
        // заголовок — только с начала строки; снимаются `\r` и хвостовые пробелы
        let l = line.trim_end();
        if let Some(p) = HEADS.iter().find_map(|h| l.strip_prefix(h)) {
            let p = p.trim();
            if !p.is_empty() && !out.iter().any(|x| x == p) {
                out.push(p.to_string());
            }
        }
    }
    out
}

/// Лексическая нормализация пути: `.` убирается, `..` сворачивает предыдущий компонент
/// (выше корня файловой системы — остаётся корень); файловая система не читается.
fn normalize(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Путь файла для масок (fix round 2): нормализованный (`.`, `..`) путь относительно корня с
/// `/`; путь вне корня (в том числе выше него через `..`) не сопоставляется — `None`.
pub fn glob_path(file: &str, cwd: &Path, root: &Path) -> Option<String> {
    let p = Path::new(file);
    let abs = normalize(&if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    });
    let norm = |p: &Path| p.to_string_lossy().replace('\\', "/");
    let (a, r) = (norm(&abs), norm(&normalize(root)));
    let r = r.trim_end_matches('/');
    let rel = if cfg!(windows) {
        strip_prefix_ci(&a, r)
    } else {
        a.strip_prefix(r)
    };
    rel.filter(|rest| rest.starts_with('/'))
        .map(|rest| rest.trim_start_matches('/').to_string())
        .filter(|rest| !rest.is_empty())
}

/// `strip_prefix` без учёта регистра (пути Windows).
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_headers() {
        let p = "*** Begin Patch\n*** Add File: scripts/new.sh\r\n+echo\n*** Update File: src/a.rs  \n\
                 @@\n-x\n+y\n*** Move to: src/b.rs\n*** Delete File: old.txt\n\
                 *** Update File: src/a.rs\n  *** Add File: indented.sh\n+ *** Add File: added-line.sh\n\
                 *** End Patch\n";
        assert_eq!(
            patch_files(p),
            ["scripts/new.sh", "src/a.rs", "src/b.rs", "old.txt"]
        );
        assert!(patch_files("ls -la").is_empty());
    }

    #[test]
    fn glob_paths_relative_to_root() {
        let root = if cfg!(windows) {
            Path::new("C:\\proj")
        } else {
            Path::new("/proj")
        };
        let f = if cfg!(windows) {
            "c:\\Proj\\scripts\\deploy.sh"
        } else {
            "/proj/scripts/deploy.sh"
        };
        let g = |f: &str, cwd: &Path| glob_path(f, cwd, root);
        assert_eq!(g(f, root).as_deref(), Some("scripts/deploy.sh"));
        assert_eq!(g("a/b.sh", &root.join("x")).as_deref(), Some("x/a/b.sh"));
        // нормализация: «.» и «..» внутри корня
        assert_eq!(g("./a/./b/../c.sh", root).as_deref(), Some("a/c.sh"));
        assert_eq!(g("../y/z.sh", &root.join("x")).as_deref(), Some("y/z.sh"));
        // выше корня и вне его — не сопоставляется
        assert_eq!(g("../a.sh", root), None);
        assert_eq!(g("x/../../a.sh", root), None);
        let outside = if cfg!(windows) {
            "D:\\o\\a.sh"
        } else {
            "/o/a.sh"
        };
        assert_eq!(g(outside, root), None);
        // префикс имени — не корень
        let sibling = if cfg!(windows) {
            "C:\\proj2\\a.sh"
        } else {
            "/proj2/a.sh"
        };
        assert_eq!(g(sibling, root), None);
        // ..-обход в абсолютном пути
        let tricky = if cfg!(windows) {
            "C:\\proj\\..\\etc\\a.sh"
        } else {
            "/proj/../etc/a.sh"
        };
        assert_eq!(g(tricky, root), None);
    }
}
