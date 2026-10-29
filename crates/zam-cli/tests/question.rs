//! `zam question` против настоящего zam-server в том же процессе тестов (M4.5 §4): постановка с
//! вариантами, дубликат (код 4, ответ отвеченного), лента, ответ (`--option`, `--text`,
//! `--channel`), повторный ответ (код 4), `--json`. Сервер без моделей.
mod common;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Output;

use zam_server::build;
use zam_server::config::ServerConfig;
use zam_store::Access;

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e51";

fn tmp(name: &str) -> PathBuf {
    common::sandbox("cli-question", name)
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

/// Каталог проекта с `solution.toml`.
fn repo(name: &str) -> PathBuf {
    let r = tmp(name);
    std::fs::create_dir_all(r.join(".zeroaimemory")).unwrap();
    std::fs::write(
        r.join(".zeroaimemory").join("solution.toml"),
        format!("agent_solution_id = \"{G}\"\nname = \"Вопросы\"\n"),
    )
    .unwrap();
    r
}

#[test]
fn ask_list_answer_cycle() {
    let (url, tok) = start("cycle");
    let r = repo("cycle-repo");
    let z = |args: &[&str], stdin: &[u8]| zam(&url, &tok, &r, args, stdin);

    // постановка: контекст из stdin, варианты и рекомендация
    let o = z(
        &[
            "question",
            "ask",
            "--title",
            "Какую базу взять?",
            "--option",
            "a=Оставить SQLite",
            "--option",
            "b=Взять Postgres",
            "--recommend",
            "a",
        ],
        "Нагрузка растёт".as_bytes(),
    );
    let created = ok(&o);
    let (qid, rev) = created.trim().split_once('@').unwrap();
    assert_eq!(rev, "1");

    // дубликат — код 4, кандидат open
    let o = z(&["question", "ask", "--title", "какую базу взять"], b"");
    assert_eq!(o.status.code(), Some(4), "{}", err(&o));
    assert!(err(&o).contains(&format!("{qid} [open]")), "{}", err(&o));

    // лента open: варианты, рекомендация звёздочкой
    let l = ok(&z(&["question", "list"], b""));
    assert!(
        l.contains(&format!(
            "{qid} [open] «Какую базу взять?» · варианты: a*=Оставить SQLite; b=Взять Postgres"
        )),
        "{l}"
    );

    // ответ: без --option/--text — код 6; неизвестный вариант — 422 (код 6)
    let o = z(&["question", "answer", &format!("q:{qid}")], b"");
    assert_eq!(o.status.code(), Some(6), "{}", err(&o));
    let o = z(
        &["question", "answer", &format!("q:{qid}"), "--option", "zzz"],
        b"",
    );
    assert_eq!(o.status.code(), Some(6), "{}{}", out(&o), err(&o));

    let a = ok(&z(
        &["question", "answer", &format!("q:{qid}"), "--option", "b"],
        b"",
    ));
    assert!(a.contains(&format!("{qid} отвечен → решение i:")), "{a}");
    let did = a.trim().rsplit("i:").next().unwrap().to_string();

    // повторный ответ — код 4 с решением первого
    let o = z(
        &[
            "question",
            "answer",
            qid,
            "--text",
            "иначе",
            "--channel",
            "chat",
        ],
        b"",
    );
    assert_eq!(o.status.code(), Some(4), "{}", err(&o));
    assert!(
        err(&o).contains(&format!("вопрос уже отвечен (решение i:{did})")),
        "{}",
        err(&o)
    );

    // отвеченный: в open-ленте нет, в answered — с ответом и решением
    assert!(!ok(&z(&["question", "list"], b"")).contains(qid));
    let l = ok(&z(&["question", "list", "--status", "answered"], b""));
    assert!(
        l.contains(&format!(
            "{qid} [answered] «Какую базу взять?» → Взять Postgres · решение i:{did}"
        )),
        "{l}"
    );

    // повтор того же заголовка: кандидат несёт ответ; --force записывает всё равно
    let o = z(&["question", "ask", "--title", "Какую базу взять?"], b"");
    assert_eq!(o.status.code(), Some(4));
    assert!(
        err(&o).contains(&format!("→ ответ: b (решение i:{did})")),
        "{}",
        err(&o)
    );
    ok(&z(
        &["question", "ask", "--title", "Какую базу взять?", "--force"],
        b"",
    ));

    // --json и ответ текстом из чата
    let q2 = ok(&z(&["question", "ask", "--title", "Нужен ли кэш?"], b""));
    let q2 = q2.trim().split_once('@').unwrap().0.to_string();
    let j: serde_json::Value = serde_json::from_str(&ok(&z(
        &[
            "question",
            "answer",
            &q2,
            "--text",
            "Да, на чтение",
            "--channel",
            "chat",
            "--json",
        ],
        b"",
    )))
    .unwrap();
    assert_eq!(j["entry"]["data"]["answer"]["channel"], "chat");
    assert_eq!(j["entry"]["data"]["answer"]["by"], "human");
    assert_eq!(j["decision"]["certainty"], "axiom");
}
