//! `solution.toml` (`agent_solution_id`, прежний `id`) и `zam solution access …` против
//! настоящего zam-server в том же процессе тестов (§11.1, §11.6).
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::Command;
use zam_server::build;
use zam_server::config::ServerConfig;
use zam_store::Access;

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e11";

fn tmp(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("cli-access-{name}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Сервер в фоновом потоке с пользователями «Анна» и «Борис»: адрес и их токены `rw`.
fn start(name: &str) -> (String, String, String) {
    let data = tmp(&format!("{name}-data"));
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let cfg = ServerConfig {
                data_dir: data,
                ..ServerConfig::default()
            };
            let (app, st, _) = build(cfg, Some("pw".into())).await.unwrap();
            let mut tokens = Vec::new();
            for n in ["Анна", "Борис"] {
                let u = st.system.create_user(n, false).await.unwrap();
                let (_, t) = st.system.issue_token(u.id, "t", Access::Rw).await.unwrap();
                tokens.push(t);
            }
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            tx.send((url, tokens[0].clone(), tokens[1].clone()))
                .unwrap();
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
    });
    rx.recv().unwrap()
}

/// `zam` с токеном `token` в каталоге `cwd`: без прокси, ZAM_TOKEN — токен пользователя.
fn zam(url: &str, token: &str, cwd: &Path, args: &[&str], stdin: &[u8]) -> Output {
    let mut c = Command::cargo_bin("zam").unwrap();
    for k in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "no_proxy",
    ] {
        c.env_remove(k);
    }
    let o = c
        .env("ZAM_URL", url)
        .env("ZAM_TOKEN", token)
        .env("HOME", cwd)
        .env("USERPROFILE", cwd)
        .current_dir(cwd)
        .args(args)
        .write_stdin(stdin.to_vec())
        .output()
        .unwrap();
    let (out, err) = (out(&o), err(&o));
    assert!(
        !out.contains(token) && !err.contains(token),
        "токен в выводе"
    );
    o
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn ok(o: &Output) -> String {
    assert_eq!(
        o.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        out(o),
        err(o)
    );
    out(o)
}

/// Каталог репозитория с `.zeroaimemory/solution.toml` из `toml`.
fn repo(root: &Path, toml: &str) -> PathBuf {
    let r = root.join("repo");
    std::fs::create_dir_all(r.join(".zeroaimemory")).unwrap();
    std::fs::write(r.join(".zeroaimemory").join("solution.toml"), toml).unwrap();
    r
}

#[test]
fn solution_toml_old_id_key_read() {
    let (url, a, _) = start("old-id");
    let root = tmp("old-id-cwd");
    // прежний ключ `id`
    let r = repo(&root, &format!("id = \"{G}\"\nname = \"Старое\"\n"));
    ok(&zam(&url, &a, &r, &["write", "x"], b"# x\n"));
    let s = ok(&zam(&url, &a, &r, &["read", &format!("{G}:x")], b""));
    assert_eq!(s, "# x\n");
    let s = ok(&zam(&url, &a, &r, &["status"], b""));
    assert!(s.contains("Роль в решении: владелец"), "{s}");
    // новый ключ `agent_solution_id` — то же решение
    let r2 = repo(
        &root.join("new"),
        &format!("agent_solution_id = \"{G}\"\nname = \"Старое\"\n"),
    );
    let s = ok(&zam(&url, &a, &r2, &["read", "x"], b""));
    assert_eq!(s, "# x\n");
    let o = zam(&url, &a, &r2, &["status", "--json"], b"");
    let v: serde_json::Value = serde_json::from_str(&ok(&o)).unwrap();
    assert_eq!(v["solution_role"], "владелец");
    assert_eq!(v["access_requests_pending"], 0);
}

#[test]
fn solution_access_flow() {
    let (url, a, b) = start("flow");
    let root = tmp("flow-cwd");
    let r = repo(
        &root,
        &format!("agent_solution_id = \"{G}\"\nname = \"Общее\"\n"),
    );
    // A регистрирует решение первой записью
    ok(&zam(
        &url,
        &a,
        &r,
        &["write", "план"],
        "# План\nшаги\n".as_bytes(),
    ));
    // B: запрос доступа, запись — 403 с объяснением
    let o = zam(&url, &b, &r, &["write", "заметка-b"], b"# B\n");
    assert_eq!(o.status.code(), Some(3), "{}", err(&o));
    assert!(err(&o).contains("ждёт подтверждения"), "{}", err(&o));
    let s = ok(&zam(&url, &b, &r, &["status"], b""));
    assert!(s.contains("нет доступа (запрос: pending)"), "{s}");
    // A видит запрос
    let s = ok(&zam(&url, &a, &r, &["status"], b""));
    assert!(
        s.contains("Ожидают подтверждения доступа: 1 (zam solution access requests)"),
        "{s}"
    );
    let s = ok(&zam(&url, &a, &r, &["solution", "access", "requests"], b""));
    assert!(s.contains("Борис") && s.contains("pending"), "{s}");
    // B управлять доступом не может
    let o = zam(&url, &b, &r, &["solution", "access", "list"], b"");
    assert_eq!(o.status.code(), Some(3), "{}", err(&o));
    // A одобряет только чтение (имя без учёта регистра)
    let s = ok(&zam(
        &url,
        &a,
        &r,
        &["solution", "access", "approve", "борис", "--ro"],
        b"",
    ));
    assert!(s.contains("approved") && s.contains("ro"), "{s}");
    let s = ok(&zam(&url, &a, &r, &["solution", "access", "requests"], b""));
    assert!(s.is_empty(), "{s}");
    let s = ok(&zam(
        &url,
        &a,
        &root,
        &["solution", "access", "list", "--solution", G, "--json"],
        b"",
    ));
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert_eq!(v["members"][0]["state"], "approved");
    // B читает, запись — 403 с текстом
    let s = ok(&zam(&url, &b, &r, &["read", "план"], b""));
    assert_eq!(s, "# План\nшаги\n");
    let s = ok(&zam(&url, &b, &r, &["status"], b""));
    assert!(s.contains("Роль в решении: участник (ro)"), "{s}");
    let o = zam(&url, &b, &r, &["write", "план"], "# правка\n".as_bytes());
    assert_eq!(o.status.code(), Some(3), "{}", err(&o));
    assert!(
        err(&o).contains("ro") && err(&o).contains("403"),
        "{}",
        err(&o)
    );
    // отзыв: следующий запрос того же токена — отказ
    ok(&zam(
        &url,
        &a,
        &r,
        &["solution", "access", "revoke", "Борис"],
        b"",
    ));
    let o = zam(&url, &b, &r, &["read", "план"], b"");
    assert_eq!(o.status.code(), Some(3), "{}", err(&o));
    assert!(err(&o).contains("отозван"), "{}", err(&o));
    // неизвестный пользователь — 5; без решения — 1
    let o = zam(
        &url,
        &a,
        &r,
        &["solution", "access", "approve", "Никто"],
        b"",
    );
    assert_eq!(o.status.code(), Some(5), "{}", err(&o));
    let o = zam(&url, &a, &root, &["solution", "access", "list"], b"");
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
}
