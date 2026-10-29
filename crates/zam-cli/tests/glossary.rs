//! `zam log term` и `zam glossary` против настоящего zam-server в том же процессе тестов
//! (M4.5 §2): аргументы записи термина, конфликт имён (код 4), список «имя — определение»,
//! карточка по имени и псевдониму, `--json`. Сервер без моделей.
mod common;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Output;

use zam_server::build;
use zam_server::config::ServerConfig;
use zam_store::Access;

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e41";

fn tmp(name: &str) -> PathBuf {
    common::sandbox("cli-glossary", name)
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

fn json(o: &Output) -> serde_json::Value {
    serde_json::from_str(&ok(o)).unwrap_or_else(|e| panic!("не JSON ({e}): {}", out(o)))
}

/// Каталог проекта с `solution.toml`.
fn repo(name: &str) -> PathBuf {
    let r = tmp(name);
    std::fs::create_dir_all(r.join(".zeroaimemory")).unwrap();
    std::fs::write(
        r.join(".zeroaimemory").join("solution.toml"),
        format!("agent_solution_id = \"{G}\"\nname = \"Словарь\"\n"),
    )
    .unwrap();
    r
}

/// Аргументы `log term`: определение, повторяемые `--alias`, `--code`, `--not-to-confuse`,
/// подробности из stdin; успех — `<id>@<rev>`; слишком длинное определение — 422 (код 6),
/// занятое имя — код 4 со списком пересечений.
#[test]
fn log_term_args() {
    let (url, tok) = start("log");
    let r = repo("log-repo");
    let z = |args: &[&str], stdin: &[u8]| zam(&url, &tok, &r, args, stdin);
    let o = z(
        &[
            "log",
            "term",
            "--title",
            "Выжимка",
            "--definition",
            "Короткая сводка записи",
            "--alias",
            "summary",
            "--alias",
            "digest",
            "--code",
            "answer_cache",
            "--not-to-confuse",
            "Заметка",
            "--json",
        ],
        "Подробности из stdin.\n".as_bytes(),
    );
    let j = json(&o);
    assert_eq!(j["created"], true);
    let e = &j["entry"];
    assert_eq!(e["kind"], "term");
    assert_eq!(e["title"], "Выжимка");
    assert_eq!(
        e["data"]["aliases"],
        serde_json::json!(["summary", "digest"])
    );
    assert_eq!(e["data"]["code"], serde_json::json!(["answer_cache"]));
    assert_eq!(e["data"]["not_to_confuse"], serde_json::json!(["Заметка"]));
    assert_eq!(e["body"], "Подробности из stdin.\n");
    // без --definition отказывает разбор аргументов
    let o = z(&["log", "term", "--title", "Т"], b"");
    assert_ne!(o.status.code(), Some(0));
    assert!(err(&o).contains("--definition"), "{}", err(&o));
    // слишком длинное определение — 422 сервера, код 6
    let long = "я".repeat(301);
    let o = z(&["log", "term", "--title", "Т", "--definition", &long], b"");
    assert_eq!(o.status.code(), Some(6), "{}", err(&o));
    assert!(err(&o).contains("data.definition"), "{}", err(&o));
    // занятое имя — код 4 и список пересечений
    let o = z(
        &[
            "log",
            "term",
            "--title",
            "Дайджест",
            "--alias",
            "SUMMARY",
            "--definition",
            "иное",
        ],
        b"",
    );
    assert_eq!(o.status.code(), Some(4), "{}", err(&o));
    assert!(
        err(&o).contains("«SUMMARY» — термин «Выжимка»"),
        "{}",
        err(&o)
    );
    // успех — короткий вывод <id>@<rev>
    let o = z(
        &[
            "log",
            "term",
            "--title",
            "Чанк",
            "--definition",
            "Кусок текста",
        ],
        b"",
    );
    assert!(ok(&o).trim().ends_with("@1"), "{}", out(&o));
}

/// `zam glossary`: список «имя — определение», карточка по имени и псевдониму, `--json`,
/// неизвестный термин — код 5.
#[test]
fn glossary_list_and_card() {
    let (url, tok) = start("card");
    let r = repo("card-repo");
    let z = |args: &[&str], stdin: &[u8]| zam(&url, &tok, &r, args, stdin);
    // пустой словарь — пустой вывод
    assert_eq!(ok(&z(&["glossary"], b"")), "");
    ok(&z(
        &[
            "log",
            "term",
            "--title",
            "Выжимка",
            "--definition",
            "Короткая сводка",
            "--alias",
            "summary",
            "--code",
            "answer_cache",
            "--not-to-confuse",
            "Заметка",
        ],
        "Подробности.\n".as_bytes(),
    ));
    ok(&z(
        &[
            "log",
            "term",
            "--title",
            "Чанк",
            "--definition",
            "Кусок текста",
        ],
        b"",
    ));
    let list = ok(&z(&["glossary"], b""));
    assert_eq!(list, "Выжимка — Короткая сводка\nЧанк — Кусок текста\n");
    // карточка по имени и по псевдониму
    for name in ["Выжимка", "summary", "SUMMARY"] {
        let card = ok(&z(&["glossary", name], b""));
        assert!(
            card.starts_with("Выжимка\nОпределение: Короткая сводка\n"),
            "{card}"
        );
        assert!(card.contains("Псевдонимы: summary\n"), "{card}");
        assert!(card.contains("Не путать с: Заметка\n"), "{card}");
        assert!(card.contains("Код: answer_cache\n"), "{card}");
        assert!(card.contains("\nПодробности.\n"), "{card}");
    }
    let j = json(&z(&["glossary", "summary", "--json"], b""));
    assert_eq!(j["kind"], "term");
    assert_eq!(j["data"]["definition"], "Короткая сводка");
    let j = json(&z(&["glossary", "--json"], b""));
    assert_eq!(j["items"].as_array().unwrap().len(), 2);
    let o = z(&["glossary", "нет такого"], b"");
    assert_eq!(o.status.code(), Some(5), "{}", err(&o));
    assert!(err(&o).contains("не найден"), "{}", err(&o));
}
