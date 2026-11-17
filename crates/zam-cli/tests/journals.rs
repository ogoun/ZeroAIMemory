//! `zam log changelog` и `zam release add|compare` против настоящего zam-server в том же
//! процессе тестов; git-репозиторий — в `CARGO_TARGET_TMPDIR` (§7.3, §7.4).
mod common;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Output;

use zam_server::build;
use zam_server::config::ServerConfig;
use zam_store::Access;

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e13";

fn tmp(name: &str) -> PathBuf {
    common::sandbox("cli-journals", name)
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

/// `git <args>` в `cwd` без глобальной конфигурации пользователя.
fn git(cwd: &Path, args: &[&str]) {
    let o = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=Тест",
            "-c",
            "user.email=t@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
        ])
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git");
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

/// Репозиторий с `solution.toml`, тремя коммитами и двумя тегами: `v1` на первом коммите,
/// `v2` на третьем (HEAD).
fn repo(name: &str) -> PathBuf {
    let r = tmp(name);
    std::fs::create_dir_all(r.join(".zeroaimemory")).unwrap();
    std::fs::write(
        r.join(".zeroaimemory").join("solution.toml"),
        format!("agent_solution_id = \"{G}\"\nname = \"Журналы\"\n"),
    )
    .unwrap();
    git(&r, &["init", "-q"]);
    git(&r, &["commit", "-q", "--allow-empty", "-m", "первый"]);
    git(&r, &["tag", "v1"]);
    git(&r, &["commit", "-q", "--allow-empty", "-m", "второй: фича"]);
    git(&r, &["commit", "-q", "--allow-empty", "-m", "третий"]);
    git(&r, &["tag", "v2"]);
    r
}

fn json(o: &Output) -> serde_json::Value {
    serde_json::from_str(&ok(o)).unwrap_or_else(|e| panic!("не JSON ({e}): {}", out(o)))
}

fn subjects(v: &serde_json::Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|c| c["subject"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn release_from_git_collects_commits() {
    let (url, tok) = start("git");
    let r = repo("git-repo");
    let z = |args: &[&str], stdin: &[u8]| zam(&url, &tok, &r, args, stdin);

    // без диапазона: от предыдущего тега (v1; v2 указывает на HEAD) до HEAD
    ok(&z(
        &["release", "add", "1.0.0", "--from-git", "--tag", "v2"],
        b"",
    ));
    let c = json(&z(&["release", "compare", "1.0.0", "--json"], b""));
    assert!(c["from"].is_null());
    assert_eq!(c["to"]["data"]["git_tag"], "v2");
    assert_eq!(subjects(&c["commits"]), ["третий", "второй: фича"]);
    assert_eq!(c["commits"][0]["sha"].as_str().unwrap().len(), 40);

    // запись журнала со ссылками, областью и текстом из stdin
    let o = z(
        &[
            "log",
            "changelog",
            "--kind",
            "feat",
            "--title",
            "Новая фича",
            "--ref",
            "commit:abc123",
            "--ref",
            "file:src/lib.rs",
            "--area",
            "web",
            "--json",
        ],
        "Описание из stdin.\n".as_bytes(),
    );
    let e = json(&o);
    assert_eq!(e["created"], true);
    assert_eq!(e["entry"]["area"], "web");
    assert_eq!(e["entry"]["body"], "Описание из stdin.\n");
    assert_eq!(e["entry"]["data"]["refs"]["commits"][0], "abc123");
    assert_eq!(e["entry"]["data"]["refs"]["files"][0], "src/lib.rs");
    // короткий вывод: <id>@<rev>
    let short = ok(&z(
        &["log", "changelog", "--kind", "fix", "--title", "Починка"],
        b"",
    ));
    assert!(short.trim().ends_with("@1"), "{short}");

    // явный диапазон и описание из файла
    let notes = r.join("notes.md");
    std::fs::write(&notes, "Описание релиза.\n").unwrap();
    ok(&z(
        &[
            "release",
            "add",
            "1.1.0",
            "--from-git",
            "v1..v2",
            "--date",
            "2026-02-03",
            "--notes-file",
            notes.to_str().unwrap(),
        ],
        b"",
    ));
    let c = json(&z(&["release", "compare", "1.1.0", "--json"], b""));
    assert_eq!(c["from"]["data"]["version"], "1.0.0");
    assert_eq!(c["to"]["data"]["date"], "2026-02-03");
    assert_eq!(subjects(&c["commits"]), ["третий", "второй: фича"]);
    assert_eq!(c["changelog"]["feat"][0]["title"], "Новая фича");
    assert_eq!(c["changelog"]["fix"][0]["title"], "Починка");
    assert!(
        c["notes_diff"]
            .as_str()
            .unwrap()
            .contains("+Описание релиза."),
        "{c}"
    );

    // текстовый вывод и сравнение с указанной версией
    let t = ok(&z(&["release", "compare", "1.1.0", "--with", "1.0.0"], b""));
    assert!(t.starts_with("# 1.0.0 → 1.1.0\n"), "{t}");
    assert!(t.contains("## feat\n- Новая фича\n"), "{t}");
    assert!(t.contains("второй: фича"), "{t}");

    // повтор версии — конфликт (4), неверная ссылка и неизвестный релиз — 6 и 5
    let o = z(&["release", "add", "1.0.0"], b"");
    assert_eq!(o.status.code(), Some(4), "{}", err(&o));
    let o = z(
        &[
            "log",
            "changelog",
            "--kind",
            "fix",
            "--title",
            "x",
            "--ref",
            "url:oops",
        ],
        b"",
    );
    // неизвестный вид ссылки — 6 (путь без вида с M4.5 — файл)
    assert_eq!(o.status.code(), Some(6), "{}", err(&o));
    let o = z(&["release", "compare", "9.9.9"], b"");
    assert_eq!(o.status.code(), Some(5), "{}", err(&o));
    // не git-диапазон — 6
    let o = z(
        &["release", "add", "2.0.0", "--from-git", "нет..такого"],
        b"",
    );
    assert_eq!(o.status.code(), Some(6), "{}", err(&o));
}

/// Каталог проекта с `solution.toml` (без git).
fn project(name: &str) -> PathBuf {
    let r = tmp(name);
    std::fs::create_dir_all(r.join(".zeroaimemory")).unwrap();
    std::fs::write(
        r.join(".zeroaimemory").join("solution.toml"),
        format!("agent_solution_id = \"{G}\"\nname = \"Журналы\"\n"),
    )
    .unwrap();
    r
}

/// M6.6 §9 (замечание 9): `zam task new` с `--owner`, `--estimate`, `--link` (повторяемый) и
/// `--priority` → `data` задачи; карточка `task show` их печатает; неверные значения — код 6
/// до запроса; `--knowledge` — синоним `--global` у `task new` и `task list`.
#[test]
fn task_new_owner_estimate_links() {
    let (url, tok) = start("task-fields");
    let r = project("task-fields-repo");
    let z = |args: &[&str]| zam(&url, &tok, &r, args, b"");
    let s = ok(&z(&[
        "task",
        "new",
        "--title",
        "Кэш сборки",
        "--next",
        "замерить",
        "--owner",
        "human",
        "--estimate",
        "90",
        "--link",
        "https://example.invalid/pr/1",
        "--link",
        "commit:abc123",
        "--priority",
        "high",
    ]));
    assert!(s.trim().ends_with("@1"), "{s}");
    let id = s.trim().split('@').next().unwrap().to_string();
    let e = json(&z(&["task", "show", &format!("k:{id}"), "--json"]));
    let d = &e["data"];
    assert_eq!(d["owner"], "human", "{e}");
    assert_eq!(d["estimate_min"], 90, "{e}");
    assert_eq!(
        d["links"],
        serde_json::json!(["https://example.invalid/pr/1", "commit:abc123"]),
        "{e}"
    );
    assert_eq!(d["priority"], "high", "{e}");
    assert_eq!(d["next_step"], "замерить", "{e}");
    assert_eq!(e["status"], "open", "{e}");
    let card = ok(&z(&["task", "show", &id]));
    for line in [
        "ведёт: human",
        "оценка: 90 мин",
        "ссылка: https://example.invalid/pr/1",
        "ссылка: commit:abc123",
    ] {
        assert!(card.contains(line), "{line}: {card}");
    }
    // неверные значения — код 6 до запроса (недоступный адрес не мешает)
    for extra in [
        ["--owner", "robot"],
        ["--estimate", "0"],
        ["--estimate", "525601"],
        ["--priority", "urgent"],
    ] {
        let args = [&["task", "new", "--title", "Т"][..], &extra].concat();
        let o = zam("http://127.0.0.1:9", "t", &r, &args, b"");
        assert_eq!(o.status.code(), Some(6), "{extra:?}: {}", err(&o));
    }
    // Knowledge: --knowledge — синоним --global
    let k = ok(&z(&[
        "task",
        "new",
        "--title",
        "Общая задача",
        "--knowledge",
    ]));
    let k = k.trim().split('@').next().unwrap().to_string();
    let l = ok(&z(&["task", "list", "--global"]));
    assert!(l.contains(&k) && !l.contains(&id), "{l}");
    assert_eq!(ok(&z(&["task", "list", "--knowledge"])), l);
}

/// Ответ заглушки ленты `kind`: первая страница — запись `01A` и `next_cursor: c1`, страница по
/// курсору — запись `01B` без курсора.
fn two_pages(kind: &str, path: &str) -> String {
    let (id, cursor) = if path.contains("cursor=c1") {
        ("01B", serde_json::Value::Null)
    } else {
        ("01A", serde_json::json!("c1"))
    };
    // статус из умолчания ленты: у идей — new, у задач и вопросов — open
    let status = if kind == "idea" { "new" } else { "open" };
    serde_json::json!({"items": [{"id": id, "status": status, "title": format!("{kind} {id}"),
                                  "data": {}}],
                       "next_cursor": cursor})
    .to_string()
}

/// Ревью задачи 8 (M2): `--all` не зацикливается на неверных курсорах сервера — тот же курсор
/// (`c1` всегда), цикл `A→B→A` и пустая страница с курсором — код 2 с причиной; запросов
/// ровно столько, сколько нужно, чтобы увидеть повтор.
#[test]
fn journal_list_all_stops_on_cursor_loop() {
    let r = project("lists-loop");
    let cases: [(&str, &str, usize); 3] = [
        ("same", "сервер повторил курсор ленты «c1»", 2),
        ("aba", "сервер повторил курсор ленты «A»", 3),
        (
            "empty",
            "сервер вернул пустую страницу ленты с курсором «c9»",
            1,
        ),
    ];
    for (mode, text, requests) in cases {
        let (url, seen) = common::stub_body(std::sync::Arc::new(
            move |_: &str, p: &str, _: usize, _: &serde_json::Value| {
                let (items, next) = match mode {
                    "same" => (serde_json::json!([{"id": "01A", "status": "open"}]), "c1"),
                    "empty" => (serde_json::json!([]), "c9"),
                    // A→B→A: первая страница → A, по A → B, по B → снова A
                    _ if p.contains("cursor=B") => {
                        (serde_json::json!([{"id": "01C", "status": "open"}]), "A")
                    }
                    _ if p.contains("cursor=A") => {
                        (serde_json::json!([{"id": "01B", "status": "open"}]), "B")
                    }
                    _ => (serde_json::json!([{"id": "01A", "status": "open"}]), "A"),
                };
                let body = serde_json::json!({"items": items, "next_cursor": next});
                (200, body.to_string(), std::time::Duration::ZERO)
            },
        ));
        let o = zam(&url, "t", &r, &["task", "list", "--all"], b"");
        assert_eq!(o.status.code(), Some(2), "{mode}: {}", err(&o));
        assert!(err(&o).contains(text), "{mode}: {}", err(&o));
        assert!(out(&o).is_empty(), "{mode}: {}", out(&o));
        assert_eq!(seen.lock().unwrap().len(), requests, "{mode}");
    }
}

/// M6.6 §9 (16г): ленты `zam task|question|idea list` — общий обход `list_entries`: `--all`
/// идёт по `next_cursor` страницами по 200 до конца; без `--all` при `next_cursor` — строка
/// stderr «показано N, есть ещё: --limit 200 или --all»; `--limit N` уходит серверу;
/// `--limit` и `--all` вместе — код 6.
#[test]
fn journal_lists_walk_cursor_and_hint() {
    let r = project("lists-stub");
    for kind in ["task", "question", "idea"] {
        let (url, seen) = common::stub_body(std::sync::Arc::new(
            move |_: &str, p: &str, _: usize, _: &serde_json::Value| {
                (200, two_pages(kind, p), std::time::Duration::ZERO)
            },
        ));
        let o = zam(&url, "t", &r, &[kind, "list", "--all"], b"");
        let s = ok(&o);
        assert!(s.contains("01A") && s.contains("01B"), "{kind}: {s}");
        assert_eq!(s.lines().count(), 2, "{kind}: {s}");
        assert!(err(&o).is_empty(), "{kind}: {}", err(&o));
        let o = zam(&url, "t", &r, &[kind, "list"], b"");
        let s = ok(&o);
        assert!(s.contains("01A") && !s.contains("01B"), "{kind}: {s}");
        assert_eq!(
            err(&o),
            "показано 1, есть ещё: --limit 200 или --all\n",
            "{kind}"
        );
        let o = zam(
            &url,
            "t",
            &r,
            &[kind, "list", "--limit", "1", "--json"],
            b"",
        );
        let j = json(&o);
        assert_eq!(j["next_cursor"], "c1", "{kind}: {j}");
        let paths: Vec<String> = seen.lock().unwrap().iter().map(|s| s.1.clone()).collect();
        assert_eq!(paths.len(), 4, "{kind}: {paths:?}");
        assert!(
            paths[0].contains("limit=200") && !paths[0].contains("cursor="),
            "{kind}: {paths:?}"
        );
        assert!(
            paths[1].contains("cursor=c1") && paths[1].contains("limit=200"),
            "{kind}: {paths:?}"
        );
        // умолчание страницы прежнее — 200
        assert!(paths[2].contains("limit=200"), "{kind}: {paths:?}");
        assert!(paths[3].contains("limit=1"), "{kind}: {paths:?}");
        assert!(
            paths
                .iter()
                .all(|p| p.contains(&format!("kind={kind}")) && p.contains(G)),
            "{kind}: {paths:?}"
        );
        let o = zam(
            "http://127.0.0.1:9",
            "t",
            &r,
            &[kind, "list", "--all", "--limit", "5"],
            b"",
        );
        assert_eq!(o.status.code(), Some(6), "{kind}: {}", err(&o));
    }
}
