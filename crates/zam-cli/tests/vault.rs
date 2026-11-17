//! `zam vault list|get|set|rm|rename|mv` против настоящего zam-server в том же процессе тестов
//! (§7.6, §13.2; M6.6 §7): значение — только из stdin, `get --raw` — ровно значение без перевода
//! строки, Vault решения из `solution.toml`, повторный `set` — новая ревизия; синонимы `add` и
//! `delete` в справке, перенос между областями, старый сервер без `vault_move`.
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

/// Окружение (M4.5 §8): `vault add --type host --title …` без значения; `--secret-stdin`
/// сохраняет значение; поиск `--q` идёт по открытым полям.
#[test]
fn vault_add_host_without_secret() {
    let (url, tok) = start("host");
    let r = tmp("repo-host");
    let z = |args: &[&str], stdin: &[u8]| zam(&url, &tok, &r, args, stdin);
    let s = ok(&z(
        &[
            "vault",
            "add",
            "--title",
            "stage",
            "--type",
            "host",
            "--purpose",
            "приёмочный стенд",
            "--address",
            "10.0.0.5:22",
            "--limits",
            "только ночью",
            "--notes",
            "ключи ротируются",
            "--global",
        ],
        b"",
    ));
    assert!(s.trim().ends_with("@1"), "{s}");
    let l = ok(&z(&["vault", "list", "--global", "--q", "приёмочный"], b""));
    assert!(
        l.contains("stage") && l.contains("host") && l.contains("10.0.0.5:22"),
        "{l}"
    );
    // значения нет: get не падает и ничего не печатает
    let o = z(&["vault", "get", "stage", "--global"], b"");
    assert_eq!(ok(&o), "");
    // --secret-stdin добавляет значение, открытые поля сохраняются
    let s = ok(&z(
        &[
            "vault",
            "add",
            "stage",
            "--type",
            "host",
            "--secret-stdin",
            "--global",
        ],
        b"s3cr3t-pass\n",
    ));
    assert!(s.trim().ends_with("@2"), "{s}");
    assert_eq!(
        ok(&z(&["vault", "get", "stage", "--global", "--raw"], b"")),
        "s3cr3t-pass"
    );
    let j: serde_json::Value =
        serde_json::from_str(&ok(&z(&["vault", "list", "--global", "--json"], b""))).unwrap();
    assert_eq!(j["items"][0]["data"]["purpose"], "приёмочный стенд", "{j}");
    // пароль без значения по-прежнему ошибка (пустой stdin)
    let o = z(
        &["vault", "set", "pw", "--type", "password", "--global"],
        b"",
    );
    assert_ne!(o.status.code(), Some(0));
}

// ---- M6.6 §7 (задача 6): синонимы, удаление, переименование, перенос между областями ----

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// Каталог проекта с `solution.toml` решения [`G`] с именем `name`.
fn repo(dir: &str, name: &str) -> PathBuf {
    let r = tmp(dir);
    std::fs::create_dir_all(r.join(".zeroaimemory")).unwrap();
    std::fs::write(
        r.join(".zeroaimemory").join("solution.toml"),
        format!("agent_solution_id = \"{G}\"\nname = \"{name}\"\n"),
    )
    .unwrap();
    r
}

/// Строка справки `zam vault --help` с подкомандой `cmd`.
fn help_line(help: &str, cmd: &str) -> String {
    help.lines()
        .find(|l| l.trim_start().starts_with(&format!("{cmd} ")))
        .unwrap_or_else(|| panic!("нет {cmd} в справке:\n{help}"))
        .to_string()
}

/// Замечание 6: `add` — видимый синоним `set` в `zam vault --help`; `rm` — с видимым синонимом
/// `delete`; `rename` и `mv` есть в справке.
#[test]
fn vault_help_shows_add_alias() {
    let o = common::zam_cmd()
        .args(["vault", "--help"])
        .write_stdin(Vec::new())
        .output()
        .unwrap();
    let help = ok(&o);
    assert!(help_line(&help, "set").contains("add"), "{help}");
    assert!(help_line(&help, "rm").contains("delete"), "{help}");
    help_line(&help, "rename");
    help_line(&help, "mv");
}

/// Замечание 7: `zam vault rename <имя> <новое>` — новая ревизия с `If-Match`, значение то же,
/// занятое имя — код 4 «имя занято» (запись прежняя), нет записи — код 5; `zam vault rm <имя>`
/// (синоним `delete`) удаляет мягко: записи нет в списке, имя свободно.
#[test]
fn vault_rm_and_rename() {
    let (url, tok) = start("rm-rename");
    let r = tmp("repo-rm");
    let z = |args: &[&str], stdin: &[u8]| zam(&url, &tok, &r, args, stdin);
    let v = format!("ghp_{}{}", "cli", "Rename-1");
    ok(&z(
        &["vault", "set", "github", "--type", "token", "--global"],
        v.as_bytes(),
    ));
    ok(&z(
        &["vault", "set", "gitlab", "--type", "token", "--global"],
        b"other-value",
    ));
    let s = ok(&z(
        &["vault", "rename", "github", "gh-main", "--global"],
        b"",
    ));
    assert!(s.trim().ends_with("@2"), "{s}");
    assert_eq!(
        ok(&z(&["vault", "get", "gh-main", "--global", "--raw"], b"")),
        v
    );
    let o = z(&["vault", "get", "github", "--global"], b"");
    assert_eq!(o.status.code(), Some(5), "{}", err(&o));
    let o = z(&["vault", "rename", "gh-main", "gitlab", "--global"], b"");
    assert_eq!(o.status.code(), Some(4), "{}", err(&o));
    assert!(err(&o).contains("имя занято"), "{}", err(&o));
    assert_eq!(
        ok(&z(&["vault", "get", "gh-main", "--global", "--raw"], b"")),
        v
    );
    let o = z(&["vault", "rename", "missing", "x", "--global"], b"");
    assert_eq!(o.status.code(), Some(5), "{}", err(&o));
    // то же имя — без новой ревизии, строка в stderr (ревью задачи 6, M4)
    let o = z(&["vault", "rename", "gh-main", "gh-main", "--global"], b"");
    assert!(ok(&o).trim().ends_with("@2"), "{}", out(&o));
    assert!(err(&o).contains("без изменений"), "{}", err(&o));
    // rm и синоним delete
    let s = ok(&z(&["vault", "rm", "gh-main", "--global"], b""));
    assert!(s.contains("удалена"), "{s}");
    let l = ok(&z(&["vault", "list", "--global"], b""));
    assert!(!l.contains("gh-main") && l.contains("gitlab"), "{l}");
    ok(&z(&["vault", "delete", "gitlab", "--global"], b""));
    assert!(
        ok(&z(&["vault", "list", "--global"], b""))
            .trim()
            .is_empty()
    );
    let o = z(&["vault", "rm", "gitlab", "--global"], b"");
    assert_eq!(o.status.code(), Some(5), "{}", err(&o));
    // имя освободилось
    let s = ok(&z(
        &["vault", "set", "gitlab", "--type", "token", "--global"],
        b"again",
    ));
    assert!(s.trim().ends_with("@1"), "{s}");
}

/// Замечание 25: `zam vault mv <имя> --to global` переносит запись Vault решения в личный Vault
/// (`<id>@<rev+1>`, значение то же); откуда — `--global`/`--solution`, `--to solution` — решение
/// из `solution.toml`, `--to <GUID>` — решение по GUID; та же область — без ревизии; неверная
/// цель — код 6 до запроса; занятое в цели имя — код 4.
#[test]
fn vault_mv_to_global() {
    let (url, tok) = start("mv");
    let r = repo("repo-mv", "Проект");
    let z = |args: &[&str], stdin: &[u8]| zam(&url, &tok, &r, args, stdin);
    let v = format!("ghp_{}{}", "cli", "Move-1");
    let s = ok(&z(
        &["vault", "set", "deploy", "--type", "token"],
        v.as_bytes(),
    ));
    assert!(s.trim().ends_with("@1"), "{s}");
    let s = ok(&z(&["vault", "mv", "deploy", "--to", "global"], b""));
    assert!(s.trim().ends_with("@2"), "{s}");
    assert_eq!(
        ok(&z(&["vault", "get", "deploy", "--global", "--raw"], b"")),
        v
    );
    let o = z(&["vault", "get", "deploy"], b"");
    assert_eq!(o.status.code(), Some(5), "{}", err(&o));
    let s = ok(&z(
        &["vault", "mv", "deploy", "--global", "--to", "solution"],
        b"",
    ));
    assert!(s.trim().ends_with("@3"), "{s}");
    assert_eq!(ok(&z(&["vault", "get", "deploy", "--raw"], b"")), v);
    // та же область по GUID — без новой ревизии
    let o = z(&["vault", "mv", "deploy", "--solution", G, "--to", G], b"");
    assert!(ok(&o).trim().ends_with("@3"), "{}", out(&o));
    assert!(err(&o).contains("без изменений"), "{}", err(&o));
    let o = z(&["vault", "mv", "deploy", "--to", "rules"], b"");
    assert_eq!(o.status.code(), Some(6), "{}", err(&o));
    // в личном Vault имя занято — код 4, запись в решении
    ok(&z(
        &["vault", "set", "deploy", "--type", "token", "--global"],
        b"personal",
    ));
    let o = z(&["vault", "mv", "deploy", "--to", "global"], b"");
    assert_eq!(o.status.code(), Some(4), "{}", err(&o));
    assert_eq!(ok(&z(&["vault", "get", "deploy", "--raw"], b"")), v);
}

/// Старый сервер (§7): 404 на `POST /vault/{id}/move`, а в `GET /health` `features` нет
/// `vault_move` (или нет самого поля) — код 1 и текст §7; сервер с флагом и 404 — код 5.
#[test]
fn vault_mv_old_server() {
    use std::sync::Arc;
    use std::time::Duration;
    const OLD: &str =
        "сервер не поддерживает перенос записей Vault: нужен zam-server 0.0.13-m6.6 или новее";
    const ID: &str = "01M7JRWG4KE4STDJNSVSZM86EV";
    let dir = tmp("repo-mv-old");
    let not_found =
        r#"{"type":"about:blank","title":"Not Found","status":404,"detail":"ресурс не найден"}"#;
    let stub = |health: &'static str| {
        common::stub_with(Arc::new(move |_: &str, p: &str, _: usize| {
            if p == "/api/v1/health" {
                (200, health, Duration::ZERO)
            } else if p.starts_with("/api/v1/spaces/knowledge/vault") {
                (
                    200,
                    r#"{"items":[{"id":"01M7JRWG4KE4STDJNSVSZM86EV","title":"github","rev":1,
                        "data":{"type":"token"},"space":"knowledge"}],"locked":false}"#,
                    Duration::ZERO,
                )
            } else {
                (404, not_found, Duration::ZERO)
            }
        }))
    };
    let mv = ["vault", "mv", "github", "--global", "--to", G];
    let run = |url: &str| {
        common::zam_cmd()
            .env("ZAM_URL", url)
            .env("ZAM_TOKEN", "t")
            .env("HOME", &dir)
            .env("USERPROFILE", &dir)
            .current_dir(&dir)
            .args(mv)
            .write_stdin(Vec::new())
            .output()
            .unwrap()
    };
    let move_path = format!("/api/v1/vault/{ID}/move");
    for health in [
        r#"{"status":"ok","version":"0.0.12-m6.5","features":["m6","idea_review"]}"#,
        r#"{"status":"ok","version":"0.0.10-m5.7"}"#,
    ] {
        let (url, seen) = stub(health);
        let o = run(&url);
        assert_eq!(o.status.code(), Some(1), "{health}: {}", err(&o));
        assert_eq!(err(&o).trim(), OLD, "{health}");
        let reqs: Vec<(String, String)> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|(m, p, _)| (m.clone(), p.clone()))
            .collect();
        assert!(
            reqs.contains(&("POST".into(), move_path.clone()))
                && reqs.contains(&("GET".into(), "/api/v1/health".into())),
            "{reqs:?}"
        );
    }
    let (url, _) =
        stub(r#"{"status":"ok","version":"0.0.13-m6.6","features":["m6","vault_move"]}"#);
    let o = run(&url);
    assert_eq!(o.status.code(), Some(5), "{}", err(&o));
    assert!(
        err(&o).contains("ресурс не найден") && !err(&o).contains("не поддерживает"),
        "{}",
        err(&o)
    );
}

/// Замечание 25: `zam vault set --type host|environment` без области — строка в stderr о Vault
/// решения и `--global`; с `--global`, `--solution` и у прочих типов строки нет; stdout —
/// по-прежнему `<id>@<rev>`.
#[test]
fn vault_set_host_hints_global() {
    let (url, tok) = start("hint");
    let r = repo("repo-hint", "Проект");
    let z = |args: &[&str], stdin: &[u8]| zam(&url, &tok, &r, args, stdin);
    let hint = |name: &str| {
        format!(
            "записано в Vault решения «Проект»; общий для проектов — --global (перенести: zam vault mv {name} --to global)"
        )
    };
    for (name, ty) in [("stage", "host"), ("bench", "environment")] {
        let o = z(
            &["vault", "set", name, "--type", ty, "--purpose", "стенд"],
            b"",
        );
        assert!(ok(&o).trim().ends_with("@1"), "{}", out(&o));
        assert!(
            err(&o).lines().any(|l| l == hint(name)),
            "{ty}: {}",
            err(&o)
        );
    }
    for args in [
        &["vault", "set", "lan", "--type", "host", "--global"][..],
        &["vault", "set", "lan2", "--type", "host", "--solution", G][..],
    ] {
        let o = z(args, b"");
        ok(&o);
        assert!(
            !err(&o).contains("записано в Vault"),
            "{args:?}: {}",
            err(&o)
        );
    }
    let o = z(&["vault", "set", "tok", "--type", "token"], b"value-1");
    ok(&o);
    assert!(!err(&o).contains("записано в Vault"), "{}", err(&o));
}
