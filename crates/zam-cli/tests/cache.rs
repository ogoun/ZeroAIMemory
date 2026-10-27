//! `zam cache` против настоящего zam-server в том же процессе тестов (§9.4, §13.2; задача 7
//! M4): выжимка из stdin с источниками, список, карточка, превращение, удаление.
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::Command;
use zam_server::build;
use zam_server::config::ServerConfig;
use zam_store::Access;

fn tmp(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("cli-cache-{name}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
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

/// Выжимка из stdin с источником `путь#секция@rev` и `n:<id>`: печатается `c_<id>`, видна в
/// списке и карточке; после правки источника — `stale` с причиной; `--promote` создаёт
/// заметку; `rm` удаляет; неверный источник — код 6.
#[test]
fn cache_put_from_stdin() {
    let (url, tok) = start("put");
    let r = tmp("put-repo");
    let z = |args: &[&str], stdin: &[u8]| zam(&url, &tok, &r, args, stdin);
    let b = ok(&z(
        &["write", "knowledge:Инструменты/Bash"],
        "# Bash

LF.

## Windows

CRLF ломает.
"
        .as_bytes(),
    ));
    let b_id = b.trim().split('@').next().unwrap().to_string();
    let c = ok(&z(
        &[
            "cache",
            "put",
            "--query",
            "Как писать bash на windows",
            "--sources",
            &format!("knowledge:/Инструменты/Bash#Bash > Windows@1,n:{b_id}"),
        ],
        "Пишите с LF: CRLF ломает bash.
"
        .as_bytes(),
    ));
    let cid = c.trim().to_string();
    assert!(cid.starts_with("c_"), "{c}");
    let list = ok(&z(&["cache", "list"], b""));
    assert!(
        list.starts_with(&format!("{cid}\tvalid\t")) && list.contains("Как писать bash на windows"),
        "{list}"
    );
    let show = ok(&z(&["cache", "show", &cid], b""));
    assert!(show.contains("Пишите с LF"), "{show}");
    assert!(
        show.contains(&format!("n:{b_id} › Bash > Windows rev1")),
        "{show}"
    );
    // правка секции Windows — выжимка stale, в карточке — причина
    ok(&z(
        &["write", "knowledge:Инструменты/Bash", "--base-rev", "1"],
        "# Bash

LF.

## Windows

CRLF ломает всё.
"
        .as_bytes(),
    ));
    let show = ok(&z(&["cache", "show", &cid], b""));
    assert!(show.contains("— stale"), "{show}");
    assert!(show.contains("rev1 (changed)"), "{show}");
    let stale = ok(&z(&["cache", "list", "--state", "stale"], b""));
    assert!(stale.starts_with(&cid), "{stale}");
    // сразу с превращением в заметку
    let p = ok(&z(
        &[
            "cache",
            "put",
            "--query",
            "LF",
            "--sources",
            &format!("n:{b_id}"),
            "--promote",
            "note:Выжимки/LF",
        ],
        "Только LF.
"
        .as_bytes(),
    ));
    let (pid, note) = p.trim().split_once('\t').unwrap();
    assert!(pid.starts_with("c_") && note.starts_with("n:"), "{p}");
    ok(&z(&["read", "knowledge:Выжимки/LF"], b""));
    ok(&z(&["cache", "rm", &cid], b""));
    let o = z(&["cache", "show", &cid], b"");
    assert_ne!(o.status.code(), Some(0));
    // нет выжимки или источника — неверный ввод (код 6)
    let o = z(
        &[
            "cache",
            "put",
            "--query",
            "q",
            "--sources",
            "n:01JAAAAAAAAAAAAAAAAAAAAAAA",
        ],
        b"x
",
    );
    assert_eq!(
        o.status.code(),
        Some(6),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let o = z(
        &[
            "cache",
            "put",
            "--query",
            "q",
            "--sources",
            &format!("n:{b_id}"),
        ],
        b"",
    );
    assert_eq!(o.status.code(), Some(6));
}
