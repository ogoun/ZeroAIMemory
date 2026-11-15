//! Текст `zam` агенту (M6 §4.4, задача 6): даты — `yyyy-MM-dd HH:mm:ss` UTC без `T`/`Z`,
//! `--json` — RFC 3339 как в API. Против настоящего zam-server в том же процессе тестов.
mod common;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::Value;

use zam_server::build;
use zam_server::config::ServerConfig;
use zam_store::Access;

fn tmp(name: &str) -> PathBuf {
    common::sandbox("cli-dates", name)
}

/// Сервер в фоновом потоке: адрес и токен `rw` пользователя.
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
        .env("ZAM_SESSION", "cli-dates-s1")
        .env("HOME", cwd)
        .env("USERPROFILE", cwd)
        .current_dir(cwd)
        .args(args)
        .write_stdin(stdin.to_vec())
        .output()
        .unwrap()
}

fn ok(o: &Output) -> String {
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    assert_eq!(
        o.status.code(),
        Some(0),
        "stdout: {out}\nstderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    out
}

/// Есть ли «dddd-dd-ddT» (дата ISO с `T`).
fn has_iso_t(s: &str) -> bool {
    let b = s.as_bytes();
    (0..b.len().saturating_sub(10)).any(|i| {
        b[i..i + 4].iter().all(u8::is_ascii_digit)
            && b[i + 4] == b'-'
            && b[i + 5..i + 7].iter().all(u8::is_ascii_digit)
            && b[i + 7] == b'-'
            && b[i + 8..i + 10].iter().all(u8::is_ascii_digit)
            && b[i + 10] == b'T'
    })
}

/// Есть ли «dddd-dd-dd dd:dd:dd» (дата и время текста агенту).
fn has_agent_ts(s: &str) -> bool {
    let b = s.as_bytes();
    let d = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    (0..b.len().saturating_sub(18)).any(|i| {
        d(i..i + 4)
            && b[i + 4] == b'-'
            && d(i + 5..i + 7)
            && b[i + 7] == b'-'
            && d(i + 8..i + 10)
            && b[i + 10] == b' '
            && d(i + 11..i + 13)
            && b[i + 13] == b':'
            && d(i + 14..i + 16)
            && b[i + 16] == b':'
            && d(i + 17..i + 19)
    })
}

/// `zam history`, `cache list|show`, `task new|claim|show` — текст без `…T…Z`, время — в виде
/// `yyyy-MM-dd HH:mm:ss`; `--json` — прежний RFC 3339.
#[test]
fn markdown_has_no_iso_t() {
    let (url, tok) = start("text");
    let r = tmp("text-repo");
    let z = |args: &[&str], stdin: &[u8]| zam(&url, &tok, &r, args, stdin);
    let b = ok(&z(
        &["write", "knowledge:Инструменты/Bash"],
        "# Bash\n\nLF.\n".as_bytes(),
    ));
    let b_id = b.trim().split('@').next().unwrap().to_string();
    let c = ok(&z(
        &[
            "cache",
            "put",
            "--query",
            "Как писать bash на windows",
            "--sources",
            &format!("n:{b_id}"),
        ],
        "Пишите с LF.\n".as_bytes(),
    ));
    let cid = c.trim().to_string();
    let t = ok(&z(
        &[
            "task",
            "new",
            "--global",
            "--title",
            "Перевести скрипты на LF",
        ],
        b"",
    ));
    let tid = t.trim().split('@').next().unwrap().to_string();
    let mut texts = Vec::new();
    for args in [
        vec!["history", "knowledge:Инструменты/Bash"],
        vec!["cache", "list"],
        vec!["cache", "show", cid.as_str()],
        vec!["task", "claim", tid.as_str(), "--ttl", "60"],
        vec!["task", "status", tid.as_str(), "in_progress"],
        vec!["task", "show", tid.as_str()],
    ] {
        let s = ok(&z(&args, b""));
        assert!(!has_iso_t(&s), "{args:?}: дата ISO с T: {s}");
        texts.push((args, s));
    }
    let find = |cmd: &str| {
        texts
            .iter()
            .find(|(a, _)| a.join(" ").starts_with(cmd))
            .map(|(_, s)| s.clone())
            .unwrap()
    };
    for cmd in ["history", "cache show", "task claim", "task show"] {
        let s = find(cmd);
        assert!(has_agent_ts(&s), "{cmd}: нет даты yyyy-MM-dd HH:mm:ss: {s}");
    }
    // `--json` — машинный контракт API: RFC 3339
    let j: Value = serde_json::from_str(&ok(&z(&["task", "show", &tid, "--json"], b""))).unwrap();
    let started = j["data"]["started_at"]
        .as_str()
        .unwrap_or_else(|| panic!("{j}"));
    assert!(started.contains('T') && started.ends_with('Z'), "{j}");
}
