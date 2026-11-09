//! Общие помощники интеграционных тестов.
#![allow(dead_code)]
use std::path::{Path, PathBuf};

pub mod api;
pub mod attach;
pub mod heavy;
pub mod ml;
pub mod process;
pub mod qdrant;
pub mod read;
pub mod reports;

use zam_server::config::ServerConfig;

/// Уникальный (по имени) пустой каталог внутри target.
pub fn tmp(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("boot-{name}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Конфигурация теста в каталоге `dir`. Рабочий процесс разбора документов — бинарник пакета:
/// текущий исполняемый файл теста подкоманды `doc-worker` не знает.
pub fn cfg_in(dir: PathBuf) -> ServerConfig {
    let mut c = ServerConfig {
        data_dir: dir,
        ..ServerConfig::default()
    };
    c.docs.worker_program = Some(PathBuf::from(env!("CARGO_BIN_EXE_zam-server")));
    // диск разработчика почти полон: запас 5 % диска отверг бы любой импорт в тестах (запас
    // 1 ГиБ остаётся; сам отказ проверяет `import_refused_when_disk_low`)
    c.import.min_free_percent = 0;
    c
}

/// Файлы каталога (рекурсивно), содержащие байты `needle` (нет каталога — пусто).
pub fn files_containing(dir: &Path, needle: &[u8]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(files_containing(&p, needle));
        } else if let Ok(b) = std::fs::read(&p)
            && b.windows(needle.len()).any(|w| w == needle)
        {
            out.push(p);
        }
    }
    out
}

/// Несуществующая ссылка того же вида, что `r`: префикс до `:` тот же, каждая цифра — другая
/// цифра, буква — другая буква того же регистра (Crockford у id, `a`–`f` у hex). Оценка токенов
/// без модели (M6, задача 5: подсерии по смене вида символа) у неё та же, что у `r`, и `~N tok`
/// ответов совпадает.
pub fn ghost_like(r: &str) -> String {
    const CROCKFORD: &[u8] = b"ABCDEFGHJKMNPQRSTVWXYZ";
    let (pre, rest) = r.rsplit_once(':').map_or(("", r), |(p, x)| (p, x));
    let body: String = rest
        .chars()
        .map(|c| match c {
            '0'..='9' => char::from(b'0' + (c as u8 - b'0' + 1) % 10),
            'a'..='f' => char::from(b'a' + (c as u8 - b'a' + 1) % 6),
            'A'..='Z' => {
                let i = CROCKFORD.iter().position(|&x| x == c as u8).unwrap_or(0);
                char::from(CROCKFORD[(i + 1) % CROCKFORD.len()])
            }
            c => c,
        })
        .collect();
    if pre.is_empty() {
        body
    } else {
        format!("{pre}:{body}")
    }
}
