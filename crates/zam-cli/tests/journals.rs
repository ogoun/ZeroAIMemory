//! `zam log changelog` и `zam release add|compare` против настоящего zam-server в том же
//! процессе тестов; git-репозиторий — в `CARGO_TARGET_TMPDIR` (§7.3, §7.4).
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::Command;
use zam_server::build;
use zam_server::config::ServerConfig;
use zam_store::Access;

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e13";

fn tmp(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("cli-journals-{name}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
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
