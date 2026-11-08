//! Общее для тестов `zam-cli` (fix round 1 задачи 8 M4.5, инцидент полного прогона): запуск
//! дочернего `zam` с чистым окружением и временные каталоги с маркером проекта.
//!
//! - [`zam_cmd`] — `zam` без системного прокси, без переменных агента
//!   ([`zam_core::AGENT_ENV_VARS`]: `CLAUDECODE`, `CLAUDE_CODE_SESSION_ID`, переменные Codex),
//!   без клиентских ([`zam_core::CLIENT_ENV_VARS`]: `ZAM_SESSION`, `ZAM_AGENT`, `ZAM_MACHINE`,
//!   `ZAM_SESSION_ID`) и
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

/// Ответ заглушки на запрос `(метод, путь, номер запроса с этим путём с 0)`: статус, тело,
/// задержка.
pub type Reply =
    dyn Fn(&str, &str, usize) -> (u16, &'static str, std::time::Duration) + Send + Sync + 'static;

/// Заглушка HTTP в фоновом потоке: на любой запрос — `status` с телом `body` (JSON) после
/// задержки `delay` (запрос сначала читается целиком); принятые запросы — в [`Seen`].
pub fn stub(status: u16, body: &'static str, delay: std::time::Duration) -> (String, Seen) {
    stub_with(std::sync::Arc::new(move |_: &str, _: &str, _: usize| {
        (status, body, delay)
    }))
}

/// Заглушка с ответом по запросу ([`Reply`]): например, ошибка на второй пачке.
pub fn stub_with(reply: std::sync::Arc<Reply>) -> (String, Seen) {
    stub_body(std::sync::Arc::new(
        move |m: &str, p: &str, n: usize, _: &serde_json::Value| {
            let (status, body, delay) = reply(m, p, n);
            (status, body.to_string(), delay)
        },
    ))
}

/// Ответ заглушки по запросу и его телу ([`stub_body`]): статус, тело, задержка.
pub type BodyReply = dyn Fn(&str, &str, usize, &serde_json::Value) -> (u16, String, std::time::Duration)
    + Send
    + Sync
    + 'static;

/// Заглушка с ответом и по телу запроса ([`BodyReply`]): например, 400 старого сервера только
/// на `POST /sessions {holdout}`, а регистрация той же сессии — 201 (M6 §12).
pub fn stub_body(reply: std::sync::Arc<BodyReply>) -> (String, Seen) {
    use std::io::{BufRead, BufReader, Read, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let seen: Seen = Default::default();
    let s2 = seen.clone();
    std::thread::spawn(move || {
        for conn in l.incoming().flatten() {
            let s3 = s2.clone();
            let reply = reply.clone();
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
                let (status, body, delay) = {
                    let mut s = s3.lock().unwrap();
                    let n = s.iter().filter(|(_, q, _)| *q == p).count();
                    let out = reply(&m, &p, n, &v);
                    s.push((m.clone(), p.clone(), v));
                    out
                };
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

/// Бинарник `zam-server` из каталога сборки — рабочий процесс разбора документов (M5). Нет его —
/// `None` с `SKIP <test>` в stderr; в CI и с `ZAM_REQUIRE_ML` пропуск недопустим (как
/// `common::ml::have` у `zam-server`).
pub fn docs_worker(test: &str) -> Option<PathBuf> {
    let worker = assert_cmd::cargo::cargo_bin("zam-server");
    if worker.is_file() {
        return Some(worker);
    }
    assert!(
        std::env::var_os("ZAM_REQUIRE_ML").is_none() && std::env::var_os("CI").is_none(),
        "нет {}: соберите cargo build -p zam-server (или cargo test --workspace)",
        worker.display()
    );
    eprintln!("SKIP {test}: нет {}", worker.display());
    None
}

/// Сервер с разбором документов на 127.0.0.1:0 в фоновом потоке (данные — `data`, рабочий
/// процесс — `worker`); адрес и первый токен администратора.
pub fn docs_server(data: PathBuf, worker: PathBuf) -> (String, String) {
    use zam_server::build;
    use zam_server::config::ServerConfig;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let mut cfg = ServerConfig {
                data_dir: data,
                ..ServerConfig::default()
            };
            cfg.docs.worker_program = Some(worker);
            let (app, _st, rep) = build(cfg, Some("pw".into())).await.unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            tx.send((url, rep.initial_token.unwrap())).unwrap();
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });
    });
    rx.recv().unwrap()
}

/// Копия `cmd::hook::session_start::holdout_pick` (M6 §12; `zam` — только бинарник): сессия `id`
/// в контрольной группе A/B при доле `p`. Совпадение закреплено значениями `PINNED_HOLDOUT`
/// юнит-теста `holdout_pick_share_and_determinism` и тестом `hooks::holdout_pick_copy_pinned`.
pub fn holdout_pick(id: &str, p: f64) -> bool {
    if p <= 0.0 {
        return false;
    }
    let mut h = blake3::Hasher::new();
    h.update(b"zam-holdout-v1\0");
    h.update(id.as_bytes());
    let b = h.finalize();
    let mut first = [0u8; 8];
    first.copy_from_slice(&b.as_bytes()[..8]);
    (u64::from_le_bytes(first) as f64 / 18_446_744_073_709_551_616.0) < p
}

/// Первая сессия `<prefix>-<n>` в контрольной группе (`in_group`) или вне её при `p`.
pub fn holdout_session(prefix: &str, p: f64, in_group: bool) -> String {
    (0..10_000)
        .map(|i| format!("{prefix}-{i}"))
        .find(|s| holdout_pick(s, p) == in_group)
        .expect("сессия группы")
}
