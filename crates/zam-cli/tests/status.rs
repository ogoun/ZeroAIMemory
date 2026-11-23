//! `zam status` против настоящего zam-server в том же процессе тестов.
mod common;
use std::net::SocketAddr;
use std::path::PathBuf;

use assert_cmd::Command;
use zam_server::build;
use zam_server::config::ServerConfig;

fn tmp(name: &str) -> PathBuf {
    common::sandbox("cli", name)
}

/// Поднимает сервер на 127.0.0.1:0; возвращает адрес и первый токен администратора.
async fn start(name: &str) -> (String, String) {
    let cfg = ServerConfig {
        data_dir: tmp(&format!("{name}-data")),
        ..common::server_defaults()
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
    let mut c = common::zam_cmd();
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
    // поля решения есть и без solution.toml (M6.6, замечание 23)
    for k in ["solution_id", "solution_name", "solution_file_name"] {
        assert!(v.as_object().unwrap().contains_key(k), "{k}: {v}");
        assert!(v[k].is_null(), "{k}: {v}");
    }
}

/// HTTP с токеном: код ответа.
fn api(url: &str, token: &str, method: &str, path: &str, body: serde_json::Value) -> u16 {
    let a = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .proxy(None)
            .http_status_as_error(false)
            .build(),
    );
    let uri = format!("{url}/api/v1{path}");
    let req = match method {
        "POST" => a.post(&uri),
        "PATCH" => a.patch(&uri),
        m => panic!("метод {m}"),
    };
    req.header("Authorization", format!("Bearer {token}"))
        .content_type("application/json")
        .send(body.to_string())
        .unwrap()
        .status()
        .as_u16()
}

/// M6.6 §9 (замечание 23): «Решение:  «<имя>» (<GUID>) — <путь>», в JSON — `solution_id` и
/// `solution_name`. Переименовано на сервере — имя реестра с пометкой; без токена (`/spaces` не
/// спрашивается) — имя из файла; файл не читается — путь и причина.
#[tokio::test(flavor = "multi_thread")]
async fn status_prints_solution_name_and_guid() {
    let (url, token) = start("solname").await;
    tokio::task::spawn_blocking(move || {
        let g = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9ea1";
        let dir = tmp("solname-cwd");
        let file = dir.join(".zeroaimemory").join("solution.toml");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(
            &file,
            format!("agent_solution_id = \"{g}\"\nname = \"Файловое\"\n"),
        )
        .unwrap();
        let run = |args: &[&str]| {
            let mut c = common::zam_cmd();
            c.env("HOME", &dir)
                .env("USERPROFILE", &dir)
                .current_dir(&dir)
                .arg("status")
                .args(["--url", &url])
                .args(args);
            c.output().unwrap()
        };
        let json = |o: &std::process::Output| -> serde_json::Value {
            serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("{e}: {}", out(o)))
        };
        let body = serde_json::json!({"agent_solution_id": g, "name": "Файловое"});
        assert_eq!(api(&url, &token, "POST", "/solutions", body), 201);
        let o = run(&["--token", &token]);
        let s = out(&o);
        assert_eq!(o.status.code(), Some(0), "{s}{}", err(&o));
        assert!(
            s.contains(&format!("Решение:  «Файловое» ({g}) — ")) && s.contains("solution.toml"),
            "{s}"
        );
        assert!(!s.contains("в solution.toml —"), "{s}");
        let v = json(&run(&["--token", &token, "--json"]));
        assert_eq!(v["solution_id"], g, "{v}");
        assert_eq!(v["solution_name"], "Файловое", "{v}");
        assert_eq!(v["solution_file_name"], "Файловое", "{v}");
        assert!(v["solution"].as_str().unwrap().ends_with("solution.toml"), "{v}");
        // переименовано в реестре (zam solution rename) — имя реестра с пометкой
        let body = serde_json::json!({"name": "Реестровое"});
        assert_eq!(api(&url, &token, "PATCH", &format!("/solutions/{g}"), body), 200);
        let s = out(&run(&["--token", &token]));
        assert!(
            s.contains(&format!("Решение:  «Реестровое» ({g}) — "))
                && s.contains("(в solution.toml — «Файловое»)"),
            "{s}"
        );
        // в JSON видны оба имени: реестра и файла (ревью задачи 10, M8)
        let v = json(&run(&["--token", &token, "--json"]));
        assert_eq!(v["solution_name"], "Реестровое", "{v}");
        assert_eq!(v["solution_file_name"], "Файловое", "{v}");
        // без токена реестр не спрашивается — имя из файла, без ошибки о решении
        let o = run(&[]);
        assert_eq!(o.status.code(), Some(3));
        assert!(
            out(&o).contains(&format!("Решение:  «Файловое» ({g}) — ")),
            "{}",
            out(&o)
        );
        // файл не читается — путь и причина
        std::fs::write(&file, "agent_solution_id = 1\n").unwrap();
        let o = run(&["--token", &token]);
        let s = out(&o);
        assert_eq!(o.status.code(), Some(0), "{s}{}", err(&o));
        assert!(
            s.lines()
                .any(|l| l.starts_with("Решение:  ") && l.contains("solution.toml — не читается: ")),
            "{s}"
        );
        let v = json(&run(&["--token", &token, "--json"]));
        assert!(v["solution_id"].is_null() && v["solution_name"].is_null(), "{v}");
        assert!(v["solution_error"].is_string(), "{v}");
    })
    .await
    .unwrap();
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

/// Домашний конфиг теста с текстом `text`; `HOME`/`USERPROFILE` команды — туда.
fn with_home(c: &mut Command, name: &str, text: &str) {
    let dir = tmp(name);
    std::fs::create_dir_all(dir.join(".zeroaimemory")).unwrap();
    std::fs::write(dir.join(".zeroaimemory/config.toml"), text).unwrap();
    c.env("HOME", &dir).env("USERPROFILE", &dir);
}

/// M7 §4.4: ключ из `[servers."<адрес>"]` — источник с адресом; адрес с «/» в конце находит тот же
/// ключ. Адрес — тестового сервера (`zam status` сначала спрашивает `/health`).
#[tokio::test(flavor = "multi_thread")]
async fn status_names_server_token_source() {
    let (url, token) = start("srvkey").await;
    let o = tokio::task::spawn_blocking({
        let (url, token) = (url.clone(), token.clone());
        move || {
            let mut c = zam("srvkey");
            with_home(
                &mut c,
                "srvkey-home",
                &format!("[servers.\"{url}\"]\ntoken = \"{token}\"\n"),
            );
            c.args(["status", "--url", &format!("{url}/")])
                .output()
                .unwrap()
        }
    })
    .await
    .unwrap();
    let s = out(&o);
    assert_eq!(o.status.code(), Some(0), "{s}{}", err(&o));
    assert!(
        s.contains(&format!(
            "Источник токена: ~/.zeroaimemory/config.toml [servers.\"{url}\"]\n"
        )),
        "{s}"
    );
    assert!(s.contains(&zam_core::mask_token(&token)), "{s}");
    assert!(!s.contains(&token) && !err(&o).contains(&token));
}

/// M7 §4.4 (решение координатора): ключ без адреса (`token` без `url`) и пустой `[servers]` —
/// ключ уходит на адрес проекта, как в 0.0.13-m6.6; примечание с командой привязки, без ключа.
#[tokio::test(flavor = "multi_thread")]
async fn status_uses_unbound_token_with_note() {
    let (url, token) = start("unbound").await;
    let o = tokio::task::spawn_blocking({
        let (url, token) = (url.clone(), token.clone());
        move || {
            let mut c = zam("unbound");
            with_home(
                &mut c,
                "unbound-home",
                &format!("token = \"{token}\"\n\n[servers]\n"),
            );
            c.args(["status", "--url", &url]).output().unwrap()
        }
    })
    .await
    .unwrap();
    let (s, e) = (out(&o), err(&o));
    assert_eq!(o.status.code(), Some(0), "{s}{e}");
    assert!(
        s.contains("Источник токена: ~/.zeroaimemory/config.toml\n"),
        "{s}"
    );
    assert!(
        e.contains("ключ без адреса")
            && e.contains(&format!("привяжите: zam init --url {url} --token")),
        "{e}"
    );
    assert!(e.contains(&zam_core::mask_token(&token)), "{e}");
    assert!(!s.contains(&token) && !e.contains(&token));
}

/// M7 §4.4: адрес без ключа, а в домашнем конфиге — ключ другого сервера: код 1 до сети с
/// подсказкой `zam init --url <адрес> --token`; ключ другого сервера не отправляется и не
/// печатается — у `zam status` и у команд заметок.
#[test]
fn missing_token_for_url_hint() {
    let other = ["zam_", &"7".repeat(40)].concat();
    let home = format!("[servers.\"http://192.0.2.1:7700\"]\ntoken = \"{other}\"\n");
    for args in [&["status"][..], &["tree"][..]] {
        let mut c = zam("nokey");
        with_home(&mut c, "nokey-home", &home);
        let o = c
            .args(args)
            .args(["--url", "http://192.0.2.2:7700"])
            .output()
            .unwrap();
        let e = err(&o);
        assert_eq!(o.status.code(), Some(1), "{args:?}: {}{e}", out(&o));
        assert!(
            e.contains("zam init --url http://192.0.2.2:7700 --token"),
            "{args:?}: {e}"
        );
        assert!(!e.contains(&other) && !out(&o).contains(&other), "{args:?}");
    }
    // `--json` — отчёт с полем `error`, как у кода 3 (круг правок 1)
    let mut c = zam("nokey");
    with_home(&mut c, "nokey-home", &home);
    let o = c
        .args(["status", "--json", "--url", "http://192.0.2.2:7700"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1), "{}{}", out(&o), err(&o));
    let v: serde_json::Value =
        serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("{e}: {}", out(&o)));
    assert_eq!(v["url"], "http://192.0.2.2:7700", "{v}");
    assert!(v["token"].is_null(), "{v}");
    assert!(
        v["error"]
            .as_str()
            .is_some_and(|e| e.contains("zam init --url http://192.0.2.2:7700 --token")),
        "{v}"
    );
    assert!(!out(&o).contains(&other));
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
