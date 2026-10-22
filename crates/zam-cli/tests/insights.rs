//! `zam log insight` и `zam insight …` против настоящего zam-server в том же процессе тестов
//! (§7.5, §13.2): триггеры, ответ 409 о дубле с подсказками, `--force`, замещение, отзыв,
//! пересмотр, дописывание, `--scope global`. Сервер без моделей (дубли — по заголовку).
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::Command;
use zam_server::build;
use zam_server::config::ServerConfig;
use zam_store::Access;

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e31";

fn tmp(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("cli-insights-{name}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Сервер в фоновом потоке: адрес и токен `rw` пользователя «Анна».
fn start(name: &str) -> (String, String) {
    let (url, tok, _) = start_human(name);
    (url, tok)
}

/// [`start`] и человеческий токен «Анны» (§7.5).
fn start_human(name: &str) -> (String, String, String) {
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
            let (_, t) = st
                .system
                .issue_token(u.id, "t", Access::Rw, false)
                .await
                .unwrap();
            let (_, h) = st
                .system
                .issue_token(u.id, "h", Access::Rw, true)
                .await
                .unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            tx.send((url, t, h)).unwrap();
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

/// Id из короткого вывода `<id>@<rev>…`.
fn id_of(s: &str) -> String {
    s.trim().split('@').next().unwrap().to_string()
}

/// `zam insight certify`: агент повышает до theorem с доказательством (без него — 422, код 6),
/// axiom без `--human` — 403 (код 3), с `--human` (human_token из
/// `~/.zeroaimemory/config.toml`) — ставится; без поля в конфигурации — ошибка токена (код 3).
#[test]
fn insight_certify_evidence_and_human_token() {
    let (url, tok, human) = start_human("certify");
    let r = tmp("certify-repo");
    let z = |args: &[&str]| zam(&url, &tok, &r, args, b"");
    let o = zam(
        &url,
        &tok,
        &r,
        &[
            "log",
            "insight",
            "--kind",
            "experiment",
            "--title",
            "Метод нерабочий",
            "--scope",
            "global",
        ],
        "Не сработал на наших данных.\n".as_bytes(),
    );
    let id = id_of(&ok(&o));
    assert_eq!(
        json(&z(&["insight", "show", &id, "--json"]))["certainty"],
        "hypothesis"
    );
    // theorem без доказательства — отказ сервера (422)
    let o = z(&[
        "insight",
        "certify",
        &id,
        "theorem",
        "--reason",
        "перепроверил",
    ]);
    assert_eq!(o.status.code(), Some(6), "{}", out(&o));
    assert!(err(&o).contains("evidence"), "{}", err(&o));
    let ev = r.join("ev.json");
    std::fs::write(
        &ev,
        r#"{"checked": "3 набора данных", "method": "повторный прогон", "result": "не работает"}"#,
    )
    .unwrap();
    let o = z(&[
        "insight",
        "certify",
        &id,
        "theorem",
        "--reason",
        "подтверждено",
        "--evidence-file",
        ev.to_str().unwrap(),
    ]);
    assert!(ok(&o).trim().ends_with("theorem"), "{}", out(&o));
    let e = json(&z(&["insight", "show", &id, "--json"]));
    assert_eq!(e["certainty"], "theorem");
    assert_eq!(e["data"]["kind"], "experiment", "data сохранена: {e}");
    assert_eq!(e["data"]["evidence"]["method"], "повторный прогон");
    // axiom агентским токеном — 403
    let o = z(&["insight", "certify", &id, "axiom", "--reason", "уверен"]);
    assert_eq!(o.status.code(), Some(3), "{}", out(&o));
    assert!(err(&o).contains("axiom"), "{}", err(&o));
    // --human без human_token в конфигурации — ошибка токена, запроса нет
    let o = z(&[
        "insight",
        "certify",
        &id,
        "axiom",
        "--reason",
        "уверен",
        "--human",
    ]);
    assert_eq!(o.status.code(), Some(3));
    assert!(err(&o).contains("human_token"), "{}", err(&o));
    // human_token в ~/.zeroaimemory/config.toml — ставится
    std::fs::create_dir_all(r.join(".zeroaimemory")).unwrap();
    std::fs::write(
        r.join(".zeroaimemory").join("config.toml"),
        format!("human_token = \"{human}\"\n"),
    )
    .unwrap();
    let o = z(&[
        "insight",
        "certify",
        &id,
        "axiom",
        "--reason",
        "утверждено",
        "--human",
    ]);
    assert!(ok(&o).trim().ends_with("axiom"), "{}", out(&o));
    // агент её больше не меняет
    let o = z(&[
        "insight",
        "certify",
        &id,
        "hypothesis",
        "--reason",
        "сомнение",
    ]);
    assert_eq!(o.status.code(), Some(3), "{}", out(&o));
}

#[test]
fn insight_log_with_triggers_dedup_and_supersede() {
    let (url, tok) = start("main");
    let r = tmp("repo");
    std::fs::create_dir_all(r.join(".zeroaimemory")).unwrap();
    std::fs::write(
        r.join(".zeroaimemory").join("solution.toml"),
        format!("agent_solution_id = \"{G}\"\nname = \"Инсайты\"\n"),
    )
    .unwrap();
    let z = |args: &[&str], stdin: &[u8]| zam(&url, &tok, &r, args, stdin);
    let title = "Bash-скрипты должны иметь LF";
    let log = |extra: &[&str], stdin: &[u8]| {
        let mut a = vec!["log", "insight", "--kind", "gotcha", "--title", title];
        a.extend_from_slice(extra);
        z(&a, stdin)
    };

    // запись с триггерами и тегом; без модели — пометка о проверке по заголовку
    let o = log(
        &[
            "--trigger",
            "file_glob:**/*.sh",
            "--trigger",
            "error_regex:bad interpreter",
            "--tag",
            "bash",
        ],
        "CRLF ломает shebang.\n".as_bytes(),
    );
    let first = id_of(&ok(&o));
    assert!(ok(&o).trim().ends_with("@1"), "{}", out(&o));
    assert!(err(&o).contains("только по заголовку"), "{}", err(&o));
    let e = json(&z(&["insight", "show", &first, "--json"], b""));
    assert_eq!(e["body"], "CRLF ломает shebang.\n");
    assert_eq!(e["tags"], serde_json::json!(["bash"]));
    assert_eq!(e["triggers"][0]["type"], "file_glob");
    assert_eq!(e["triggers"][0]["pattern"], "**/*.sh");
    assert_eq!(e["triggers"][1]["pattern"], "bad interpreter");
    let card = ok(&z(&["insight", "show", &first], b""));
    assert!(card.contains("[active] gotcha") && card.contains("триггер: file_glob:**/*.sh"));

    // дубль: код 4, кандидаты и подсказки
    let o = log(&[], b"");
    assert_eq!(o.status.code(), Some(4), "{}", err(&o));
    let e2 = err(&o);
    assert!(e2.contains(&first) && e2.contains("— title"), "{e2}");
    assert!(
        e2.contains("--supersedes") && e2.contains("--force") && e2.contains("zam insight append"),
        "{e2}"
    );
    // неверный триггер — код 6 до запроса
    let o = log(&["--trigger", "glob:*.sh"], b"");
    assert_eq!(o.status.code(), Some(6), "{}", err(&o));

    // --force записывает дубль; --supersedes замещает исходную запись
    let forced = id_of(&ok(&log(&["--force"], "Повтор.".as_bytes())));
    assert_ne!(forced, first);
    let o = log(&["--supersedes", &first], "Уточнение.".as_bytes());
    let newer = id_of(&ok(&o));
    let old = json(&z(&["insight", "show", &first, "--json"], b""));
    assert_eq!(old["status"], "superseded");
    assert_eq!(old["superseded_by_id"], newer.as_str());

    // zam insight supersede: вид по умолчанию — у замещаемой
    let o = z(
        &[
            "insight",
            "supersede",
            &newer,
            "--title",
            "Shell-скрипты: только LF",
            "--trigger",
            "keyword:crlf",
        ],
        "Итог.".as_bytes(),
    );
    let s = ok(&o);
    assert!(s.contains(&format!("(замещает {newer})")), "{s}");
    let latest = id_of(&s);
    let l = json(&z(&["insight", "show", &latest, "--json"], b""));
    assert_eq!(l["data"]["kind"], "gotcha");
    assert_eq!(l["supersedes_id"], newer.as_str());
    assert_eq!(l["triggers"][0]["pattern"], "crlf");

    // лента по статусу
    let lst = ok(&z(&["insight", "list", "--status", "superseded"], b""));
    assert!(lst.contains(&first) && lst.contains(&newer), "{lst}");
    assert!(!lst.contains(&latest), "{lst}");

    // пересмотр, дописывание, отзыв
    let o = ok(&z(
        &["insight", "revisit", &forced, "--reason", "проверить"],
        b"",
    ));
    assert!(o.trim().ends_with("revisit"), "{o}");
    let o = ok(&z(
        &["insight", "append", &forced],
        "Дополнение.".as_bytes(),
    ));
    assert!(o.contains("@3"), "{o}");
    let f = json(&z(&["insight", "show", &forced, "--json"], b""));
    assert_eq!(f["body"], "Повтор.\n\nДополнение.");
    let o = z(&["insight", "retract", &forced, "--reason", "дубль"], b"");
    assert!(ok(&o).trim().ends_with("retracted"), "{}", out(&o));
    let o = z(&["insight", "retract", &forced, "--reason", "ещё"], b"");
    assert_eq!(o.status.code(), Some(4), "{}", err(&o));
    let o = z(&["insight", "append", &latest], b"");
    assert_eq!(o.status.code(), Some(6), "{}", err(&o));

    // --scope global — в Knowledge
    let g = id_of(&ok(&z(
        &[
            "log",
            "insight",
            "--kind",
            "pattern",
            "--title",
            "Глобальный приём",
            "--scope",
            "global",
        ],
        b"",
    )));
    let lst = ok(&z(&["insight", "list", "--global"], b""));
    assert!(lst.contains(&g) && !lst.contains(&first), "{lst}");
    let lst = json(&z(&["insight", "list", "--json"], b""));
    assert!(
        !lst["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["id"] == g.as_str())
    );
}
