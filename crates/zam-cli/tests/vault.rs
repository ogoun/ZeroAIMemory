//! `zam vault list|get|set` против настоящего zam-server в том же процессе тестов (§7.6,
//! §13.2): значение — только из stdin, `get --raw` — ровно значение без перевода строки,
//! Vault решения из `solution.toml`, повторный `set` — новая ревизия.
mod common;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Output;

use zam_server::build;
use zam_server::config::ServerConfig;
use zam_store::Access;

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e61";

fn tmp(name: &str) -> PathBuf {
    common::sandbox("cli-vault", name)
}

/// Сервер в фоновом потоке: адрес и токен `rw` пользователя «Анна».
fn start(name: &str) -> (String, String) {
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
            let u = st.system.create_user("Анна", false).await.unwrap();
            let (_, t) = st.system.issue_token(u.id, "t", Access::Rw).await.unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            tx.send((url, t)).unwrap();
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

fn zam(url: &str, token: &str, cwd: &Path, args: &[&str], stdin: &[u8]) -> Output {
    let mut c = common::zam_cmd();
    c.env("ZAM_URL", url)
        .env("ZAM_TOKEN", token)
        .env("HOME", cwd)
        .env("USERPROFILE", cwd)
        .current_dir(cwd)
        .args(args)
        .write_stdin(stdin.to_vec())
        .output()
        .unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn ok(o: &Output) -> String {
    assert_eq!(
        o.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        out(o),
        String::from_utf8_lossy(&o.stderr)
    );
    out(o)
}

#[test]
fn vault_set_from_stdin_get_raw() {
    let (url, tok) = start("main");
    let r = tmp("repo");
    let z = |args: &[&str], stdin: &[u8]| zam(&url, &tok, &r, args, stdin);
    let v1 = format!("ghp_{}{}", "cli", "Secret-1");
    // значение из stdin, один перевод строки отбрасывается
    let o = z(
        &[
            "vault",
            "set",
            "github",
            "--type",
            "token",
            "--link",
            "https://github.com",
            "--global",
        ],
        format!("{v1}\n").as_bytes(),
    );
    let s = ok(&o);
    assert!(s.trim().ends_with("@1"), "{s}");
    assert!(!s.contains(&v1));
    // get --raw — ровно значение; без --raw — с переводом строки
    assert_eq!(
        ok(&z(&["vault", "get", "github", "--global", "--raw"], b"")),
        v1
    );
    assert_eq!(
        ok(&z(&["vault", "get", "github", "--global"], b"")),
        format!("{v1}\n")
    );
    // список: имя, тип, адрес — без значения
    let l = ok(&z(&["vault", "list", "--global"], b""));
    assert!(
        l.contains("github") && l.contains("token") && l.contains("https://github.com"),
        "{l}"
    );
    assert!(!l.contains(&v1), "{l}");
    let l = ok(&z(&["vault", "list", "--global", "--q", "нет-такого"], b""));
    assert!(l.trim().is_empty(), "{l}");
    // повторный set — новая ревизия; открытые поля сохраняются
    let v2 = format!("ghp_{}{}", "cli", "Secret-2");
    let s = ok(&z(
        &["vault", "set", "github", "--type", "token", "--global"],
        v2.as_bytes(),
    ));
    assert!(s.trim().ends_with("@2"), "{s}");
    assert_eq!(
        ok(&z(&["vault", "get", "github", "--global", "--raw"], b"")),
        v2
    );
    let j: serde_json::Value =
        serde_json::from_str(&ok(&z(&["vault", "list", "--global", "--json"], b""))).unwrap();
    assert_eq!(j["items"][0]["data"]["url"], "https://github.com", "{j}");
    // значение из аргументов не принимается (код 6), пустое значение — тоже
    let o = z(
        &[
            "vault",
            "set",
            "x",
            "value-in-args",
            "--type",
            "token",
            "--global",
        ],
        b"",
    );
    assert_eq!(o.status.code(), Some(6));
    let o = z(&["vault", "set", "x", "--type", "token", "--global"], b"");
    assert_eq!(
        o.status.code(),
        Some(6),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    // нет записи — код 5
    let o = z(&["vault", "get", "missing", "--global"], b"");
    assert_eq!(o.status.code(), Some(5));
    // без solution.toml и --global — ошибка конфигурации
    let o = z(&["vault", "list"], b"");
    assert_eq!(o.status.code(), Some(1));
    // Vault решения из solution.toml: отдельная область с тем же именем
    std::fs::create_dir_all(r.join(".zeroaimemory")).unwrap();
    std::fs::write(
        r.join(".zeroaimemory").join("solution.toml"),
        format!("agent_solution_id = \"{G}\"\nname = \"Vault\"\n"),
    )
    .unwrap();
    let s = ok(&z(
        &[
            "vault",
            "set",
            "github",
            "--type",
            "password",
            "--username",
            "bot",
        ],
        b"sol-value",
    ));
    assert!(s.trim().ends_with("@1"), "{s}");
    assert_eq!(
        ok(&z(&["vault", "get", "github", "--raw"], b"")),
        "sol-value"
    );
    assert_eq!(
        ok(&z(&["vault", "get", "github", "--global", "--raw"], b"")),
        v2
    );
}
