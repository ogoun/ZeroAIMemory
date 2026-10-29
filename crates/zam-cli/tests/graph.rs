//! `zam link` и `zam links` против настоящего zam-server в том же процессе тестов (§8.5,
//! задача 10 M3): создание явной связи по пути и `[[Имя]]`, список исходящих и обратных,
//! удаление, отказ для `supersedes`.
mod common;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Output;

use zam_server::build;
use zam_server::config::ServerConfig;
use zam_store::Access;

fn tmp(name: &str) -> PathBuf {
    common::sandbox("cli-graph", name)
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
fn link_create_list_remove() {
    let (url, tok) = start("crud");
    let r = tmp("crud-repo");
    let z = |args: &[&str], stdin: &[u8]| zam(&url, &tok, &r, args, stdin);
    let b = ok(&z(&["write", "knowledge:notes/b"], b"# B\n"));
    let b_id = b.trim().split('@').next().unwrap().to_string();
    ok(&z(
        &["write", "knowledge:notes/a"],
        "# A\n\n[[b]] [[Нет]]\n".as_bytes(),
    ));
    ok(&z(&["write", "knowledge:notes/c"], b"# C\n"));

    // путь источника, [[Имя]] цели
    let edge = ok(&z(
        &["link", "knowledge:notes/c", "depends_on", "[[b]]"],
        b"",
    ));
    let edge = edge.trim().to_string();
    assert_eq!(edge.len(), 26, "{edge}");

    let links = ok(&z(&["links", "knowledge:notes/a"], b""));
    assert!(
        links.contains(&format!("→\tlinks_to\tn:{b_id}\tnotes/b\twikilink\t")),
        "{links}"
    );
    assert!(
        links.contains("→\tlinks_to\t-\t[[Нет]]\twikilink\t"),
        "{links}"
    );
    let back = ok(&z(&["links", &format!("n:{b_id}")], b""));
    assert!(back.contains("←\tlinks_to\t"), "{back}");
    assert!(back.contains("←\tdepends_on\t"), "{back}");
    assert!(back.contains(&edge), "{back}");
    let j: serde_json::Value =
        serde_json::from_str(&ok(&z(&["links", "knowledge:notes/b", "--json"], b""))).unwrap();
    assert_eq!(j["backlinks"].as_array().unwrap().len(), 2, "{j}");

    // supersedes — только замещением: 422 → код 6; неверный ввод — код 6
    let o = z(&["link", "knowledge:notes/c", "supersedes", "[[b]]"], b"");
    assert_eq!(o.status.code(), Some(6));
    let o = z(&["link", "knowledge:notes/c", "relates_to"], b"");
    assert_eq!(o.status.code(), Some(6));

    ok(&z(&["link", "rm", &edge], b""));
    let o = z(&["link", "rm", &edge], b"");
    assert_eq!(o.status.code(), Some(5));
    let back = ok(&z(&["links", &format!("n:{b_id}")], b""));
    assert!(!back.contains("depends_on"), "{back}");
}
