//! `zam sessions tokens` против настоящего zam-server в том же процессе тестов (M6 §8, задача
//! 8b): таблица «событие · вызовов · токенов · уже выдавалось» одной сессии, сумма по
//! `--last N`, `--by-holdout`, `--json` и сессия агента из `ZAM_SESSION`.
mod common;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::Value;

use zam_server::build;
use zam_server::config::ServerConfig;
use zam_store::{Access, SessionTouch, TokenCount};

fn tmp(name: &str) -> PathBuf {
    common::sandbox("cli-sessions", name)
}

fn count(event: &str, calls: u64, tokens: u64, reread: u64) -> TokenCount {
    TokenCount {
        event: event.into(),
        calls,
        tokens,
        reread_calls: reread,
    }
}

/// Сервер в фоновом потоке с учётом двух сессий (`s-old` — holdout, затем `s-new`): адрес и
/// токен `rw` пользователя.
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
            let db = st.users.get(u.id).await.unwrap();
            let touch = |id: &str| SessionTouch {
                id: id.into(),
                agent: Some("claude".into()),
                ..SessionTouch::default()
            };
            zam_store::add_token_counts(
                &db,
                &touch("s-old"),
                &[count("session_start", 1, 500, 0), count("read", 2, 80, 0)],
            )
            .await
            .unwrap();
            zam_store::set_holdout(&db, "s-old", true).await.unwrap();
            // последнее обращение позже: `s-new` — свежая
            std::thread::sleep(std::time::Duration::from_millis(20));
            zam_store::add_token_counts(
                &db,
                &touch("s-new"),
                &[
                    count("session_start", 1, 700, 0),
                    count("rules", 1, 300, 0),
                    count("read", 4, 400, 1),
                    count("mcp", 3, 90, 2),
                ],
            )
            .await
            .unwrap();
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

fn zam(url: &str, token: &str, cwd: &Path, args: &[&str], session: Option<&str>) -> Output {
    let mut c = common::zam_cmd();
    c.env("ZAM_URL", url)
        .env("ZAM_TOKEN", token)
        .env("HOME", cwd)
        .env("USERPROFILE", cwd)
        .current_dir(cwd)
        .args(args);
    if let Some(s) = session {
        c.env("ZAM_SESSION", s);
    }
    c.output().unwrap()
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

/// Одна сессия по id и сессия агента (`ZAM_SESSION`): строка сессии, таблица по событиям в
/// порядке событий, итог и доля «уже выдавалось» у `read` и `mcp`; неизвестная сессия — код 5.
#[test]
fn cli_sessions_tokens_table() {
    let (url, tok) = start("table");
    let r = tmp("table-repo");
    let want = "событие\tвызовов\tтокенов\tуже выдавалось\n\
                session_start\t1\t700\t\n\
                rules\t1\t300\t\n\
                read\t4\t400\t1\n\
                mcp\t3\t90\t2\n\
                итого\t\t1490\t\n\
                уже выдавалось: read 1 из 4 (25 %); mcp 2 из 3 (67 %)\n";
    let out = ok(&zam(&url, &tok, &r, &["sessions", "tokens", "s-new"], None));
    let (head, table) = out.split_once('\n').unwrap();
    assert!(
        head.starts_with("сессия s-new · claude · начало ")
            && head.ends_with(" · эпоха 0")
            && !head.contains('T'),
        "{out}"
    );
    assert_eq!(table, want, "{out}");
    // без id — сессия агента
    let cur = ok(&zam(&url, &tok, &r, &["sessions", "tokens"], Some("s-new")));
    assert_eq!(cur, out);
    let j: Value = serde_json::from_str(&ok(&zam(
        &url,
        &tok,
        &r,
        &["sessions", "tokens", "s-new", "--json"],
        None,
    )))
    .unwrap();
    assert_eq!(
        j["groups"]["all"]["tokens"]["read"]["reread_calls"], 1,
        "{j}"
    );
    assert_eq!(j["items"][0]["id"], "s-new", "{j}");
    let o = zam(&url, &tok, &r, &["sessions", "tokens", "s-none"], None);
    assert_eq!(o.status.code(), Some(5), "{o:?}");
}

/// `--last N` — сумма по N последним сессиям; `--by-holdout` — отдельно основная и контрольная
/// группы; `--last` и id вместе — ошибка ввода.
#[test]
fn cli_sessions_tokens_last() {
    let (url, tok) = start("last");
    let r = tmp("last-repo");
    let out = ok(&zam(
        &url,
        &tok,
        &r,
        &["sessions", "tokens", "--last", "2"],
        None,
    ));
    assert!(out.starts_with("сессий: 2 · с "), "{out}");
    assert!(out.contains("\nsession_start\t2\t1200\t\n"), "{out}");
    assert!(out.contains("\nread\t6\t480\t1\n"), "{out}");
    assert!(out.contains("уже выдавалось: read 1 из 6 (17 %)"), "{out}");
    // одна последняя — свежая
    let one = ok(&zam(
        &url,
        &tok,
        &r,
        &["sessions", "tokens", "--last", "1"],
        None,
    ));
    assert!(one.starts_with("сессия s-new "), "{one}");
    let by = ok(&zam(
        &url,
        &tok,
        &r,
        &["sessions", "tokens", "--last", "2", "--by-holdout"],
        None,
    ));
    let (main, hold) = by
        .split_once("\nконтрольная группа (holdout): сессий 1\n")
        .unwrap_or_else(|| panic!("{by}"));
    assert!(main.contains("основная группа: сессий 1\n"), "{by}");
    assert!(main.contains("\nsession_start\t1\t700\t\n"), "{by}");
    assert!(hold.contains("session_start\t1\t500\t\n"), "{by}");
    assert!(hold.contains("read\t2\t80\t\n"), "{by}");
    let o = zam(
        &url,
        &tok,
        &r,
        &["sessions", "tokens", "s-new", "--last", "2"],
        None,
    );
    assert_eq!(o.status.code(), Some(6), "{o:?}");
}
