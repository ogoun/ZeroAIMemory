//! `zam status` против настоящего zam-server в том же процессе тестов.
use std::net::SocketAddr;
use std::path::PathBuf;

use assert_cmd::Command;
use zam_server::build;
use zam_server::config::ServerConfig;

fn tmp(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("cli-{name}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Поднимает сервер на 127.0.0.1:0; возвращает адрес и первый токен администратора.
async fn start(name: &str) -> (String, String) {
    let cfg = ServerConfig {
        data_dir: tmp(&format!("{name}-data")),
        ..ServerConfig::default()
    };
    let (app, _st, rep) = build(cfg, Some("pw".into())).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    (url, rep.initial_token.unwrap())
}

/// `zam` с чистым окружением: без ZAM_*, HOME и cwd — во временном каталоге.
fn zam(name: &str) -> Command {
    let dir = tmp(&format!("{name}-cwd"));
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
    c.env_remove("ZAM_URL")
        .env_remove("ZAM_TOKEN")
        .env("HOME", &dir)
        .env("USERPROFILE", &dir)
        .current_dir(&dir);
    c
}

fn out(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn version_prints_name_version_git() {
    let o = zam("version").arg("--version").output().unwrap();
    let s = out(&o);
    assert!(o.status.success());
    assert!(
        s.starts_with(&format!("zam {} (", env!("CARGO_PKG_VERSION"))),
        "{s}"
    );
    assert!(s.trim_end().ends_with(')'));
}

#[tokio::test(flavor = "multi_thread")]
async fn status_ok_prints_mask_not_token() {
    let (url, token) = start("ok").await;
    let o = tokio::task::spawn_blocking({
        let (url, token) = (url.clone(), token.clone());
        move || {
            zam("ok")
                .args(["status", "--url", &url, "--token", &token])
                .output()
                .unwrap()
        }
    })
    .await
    .unwrap();
    let s = out(&o);
    assert_eq!(o.status.code(), Some(0), "{s}{}", err(&o));
    assert!(s.contains(&url), "{s}");
    assert!(s.contains("Источник: flag"), "{s}");
    assert!(s.contains(&zam_core::mask_token(&token)), "{s}");
    assert!(s.contains("доступ admin") || s.contains("доступ rw"), "{s}");
    assert!(s.contains("Пользователь:"), "{s}");
    assert!(s.contains("Решение:  не найдено"), "{s}");
    assert!(!s.contains(&token) && !err(&o).contains(&token));
}

#[tokio::test(flavor = "multi_thread")]
async fn status_json_has_fields_and_only_mask() {
    let (url, token) = start("json").await;
    let o = tokio::task::spawn_blocking({
        let (url, token) = (url.clone(), token.clone());
        move || {
            zam("json")
                .args(["status", "--json", "--url", &url, "--token", &token])
                .output()
                .unwrap()
        }
    })
    .await
    .unwrap();
    assert_eq!(o.status.code(), Some(0));
    let s = out(&o);
    assert!(!s.contains(&token));
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert_eq!(v["url"], url);
    assert_eq!(v["token"], zam_core::mask_token(&token));
    assert!(v["server"]["version"].is_string());
    assert!(v["user"]["id"].is_string() && v["user"]["name"].is_string());
    assert!(v["solution"].is_null());
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_token_exit_3() {
    let (url, _token) = start("badtoken").await;
    let bad = "zam_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghij0123";
    let o = tokio::task::spawn_blocking({
        let url = url.clone();
        move || {
            zam("badtoken")
                .args(["status", "--url", &url, "--token", bad])
                .output()
                .unwrap()
        }
    })
    .await
    .unwrap();
    assert_eq!(o.status.code(), Some(3), "{}", out(&o));
    assert!(!out(&o).contains(bad) && !err(&o).contains(bad));
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_token_exit_3_prints_server() {
    let (url, _token) = start("notoken").await;
    let o = tokio::task::spawn_blocking(move || {
        zam("notoken")
            .args(["status", "--url", &url])
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    assert_eq!(o.status.code(), Some(3));
    let s = out(&o);
    assert!(
        s.contains("Сервер:") && s.contains("Токен:    не задан"),
        "{s}"
    );
}

#[test]
fn unreachable_server_exit_2() {
    // порт освобождаем сразу после выбора: слушателя нет
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let o = zam("down")
        .args([
            "status",
            "--url",
            &format!("http://127.0.0.1:{port}"),
            "--token",
            "zam_x",
        ])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
}

#[test]
fn no_url_exit_1_with_hint() {
    let o = zam("nourl").arg("status").output().unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("не задан адрес сервера"), "{}", err(&o));
}

#[test]
fn bad_url_scheme_exit_1() {
    for u in ["host:7700", "ftp://h"] {
        let o = zam("badscheme")
            .args(["status", "--url", u])
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(1), "{u}");
        assert!(err(&o).contains("http:// или https://"), "{}", err(&o));
    }
}

#[test]
fn broken_home_config_exit_1_with_path() {
    let mut c = zam("brokenhome");
    let dir = tmp("brokenhome-cwd-keep");
    std::fs::create_dir_all(dir.join(".zeroaimemory")).unwrap();
    std::fs::write(dir.join(".zeroaimemory/config.toml"), "url = [").unwrap();
    c.env("HOME", &dir).env("USERPROFILE", &dir);
    let o = c.arg("status").output().unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("config.toml"), "{}", err(&o));
}
