//! Общее для тестов `zam-cli` (fix round 1 задачи 8 M4.5, инцидент полного прогона): запуск
//! дочернего `zam` с чистым окружением и временные каталоги с маркером проекта.
//!
//! - [`zam_cmd`] — `zam` без системного прокси, без переменных агента
//!   ([`zam_core::AGENT_ENV_VARS`]: `CLAUDECODE`, `CLAUDE_CODE_SESSION_ID`, переменные Codex),
//!   без клиентских ([`zam_core::CLIENT_ENV_VARS`]: `ZAM_SESSION`, `ZAM_AGENT`, `ZAM_MACHINE`) и
//!   без любых других `ZAM_*` процесса тестов; нужные значения тест задаёт сам после вызова.
//!   Иначе `zam`, запущенный из сессии агента, писал бы в журнал этой сессии.
//! - [`sandbox`] — каталог теста `<CARGO_TARGET_TMPDIR>/<префикс>-<имя>/w`, а уровнем выше —
//!   файл `.git` (как у worktree, с несуществующим `gitdir`): корень проекта (`project_root`) не
//!   поднимается до корня репозитория ZeroAIMemory, даже когда `w` — «домашний» каталог теста
//!   (`.zeroaimemory/` домашнего каталога маркером не считается), а свой `.git` или
//!   `.zeroaimemory/` внутри `w` по-прежнему ближе. git в таком каталоге без своего репозитория
//!   отвечает ошибкой, как вне репозитория.
//! - [`repo_root`] и [`REPO_LITTER`] — для теста-сторожа чистоты корня репозитория.
#![allow(dead_code)]

use std::path::PathBuf;

use assert_cmd::Command;

/// Переменные системного прокси (снимаются: тесты ходят на 127.0.0.1).
pub const PROXY_VARS: [&str; 8] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
];

/// `zam` с чистым окружением (см. модуль).
pub fn zam_cmd() -> Command {
    let mut c = Command::cargo_bin("zam").unwrap();
    for k in PROXY_VARS {
        c.env_remove(k);
    }
    for k in zam_core::AGENT_ENV_VARS
        .iter()
        .chain(&zam_core::CLIENT_ENV_VARS)
    {
        c.env_remove(k);
    }
    for (k, _) in std::env::vars_os() {
        if let Some(k) = k.to_str()
            && k.starts_with("ZAM_")
        {
            c.env_remove(k);
        }
    }
    c
}

/// Каталог теста с маркером проекта уровнем выше (см. модуль); пересоздаётся.
pub fn sandbox(prefix: &str, name: &str) -> PathBuf {
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("{prefix}-{name}"));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    std::fs::write(
        base.join(".git"),
        "gitdir: ./zam-test-no-repo
",
    )
    .unwrap();
    let d = base.join("w");
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Корень репозитория ZeroAIMemory.
pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// Что тесты не должны оставлять в корне репозитория (инцидент полного прогона задачи 8).
pub const REPO_LITTER: [&str; 6] = [
    ".zeroaimemory",
    "CLAUDE.md",
    "AGENTS.md",
    ".mcp.json",
    ".codex",
    ".claude/settings.json",
];

/// Запрос, принятый заглушкой: метод, путь, тело (JSON или `Null`).
pub type Seen = std::sync::Arc<std::sync::Mutex<Vec<(String, String, serde_json::Value)>>>;

/// Заглушка HTTP в фоновом потоке: на любой запрос — `status` с телом `body` (JSON) после
/// задержки `delay` (запрос сначала читается целиком); принятые запросы — в [`Seen`].
pub fn stub(status: u16, body: &'static str, delay: std::time::Duration) -> (String, Seen) {
    use std::io::{BufRead, BufReader, Read, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let seen: Seen = Default::default();
    let s2 = seen.clone();
    std::thread::spawn(move || {
        for conn in l.incoming().flatten() {
            let s3 = s2.clone();
            std::thread::spawn(move || {
                let mut r = BufReader::new(conn.try_clone().unwrap());
                let mut line = String::new();
                if r.read_line(&mut line).is_err() {
                    return;
                }
                let mut parts = line.split_whitespace();
                let (m, p) = (
                    parts.next().unwrap_or("").to_string(),
                    parts.next().unwrap_or("").to_string(),
                );
                let mut len = 0usize;
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).is_err() || h.trim().is_empty() {
                        break;
                    }
                    if let Some((k, v)) = h.split_once(':')
                        && k.eq_ignore_ascii_case("content-length")
                    {
                        len = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut b = vec![0u8; len];
                let _ = r.read_exact(&mut b);
                let v = serde_json::from_slice(&b).unwrap_or(serde_json::Value::Null);
                s3.lock().unwrap().push((m, p, v));
                std::thread::sleep(delay);
                let mut c = conn;
                let _ = write!(
                    c,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            });
        }
    });
    (url, seen)
}
