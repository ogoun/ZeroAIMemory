//! Ссылки на файлы со стороны клиента (M4.5 §6, задача 8): `zam log … --ref` с хэшем и
//! коммитом, `zam write --ref` (frontmatter и подтверждение хэша), `zam refs check [--since]`,
//! `zam refs verify`, нормализация путей (Windows: регистр и `\`). Сервер — настоящий
//! zam-server в процессе тестов (без моделей), кроме проверки тела запроса — там заглушка
//! HTTP. Каталоги — в `CARGO_TARGET_TMPDIR`.
mod common;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::{Value, json};
use zam_server::build;
use zam_server::config::ServerConfig;
use zam_store::Access;

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e62";

fn tmp(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("cli-refs-{name}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn http() -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .proxy(None)
            .http_status_as_error(false)
            .build(),
    )
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

/// Заглушка HTTP: на любой запрос — `201 {"id": "01X", "rev": 1}`.
fn stub() -> (String, common::Seen) {
    common::stub(201, r#"{"id":"01X","rev":1}"#, std::time::Duration::ZERO)
}

struct Proj {
    url: String,
    token: String,
    repo: PathBuf,
    home: PathBuf,
}

impl Proj {
    fn new(name: &str, url: &str, token: &str) -> Proj {
        let root = tmp(name);
        let repo = root.join("repo");
        let home = root.join("home");
        std::fs::create_dir_all(repo.join(".zeroaimemory")).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            repo.join(".zeroaimemory/solution.toml"),
            format!("agent_solution_id = \"{G}\"\nname = \"Ссылки\"\n"),
        )
        .unwrap();
        Proj {
            url: url.into(),
            token: token.into(),
            repo,
            home,
        }
    }

    fn file(&self, rel: &str, body: &[u8]) {
        let p = self.repo.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    /// `zam <args>` с stdin в каталоге `cwd` (относительно корня проекта).
    fn zam_in(&self, cwd: &str, args: &[&str], stdin: &[u8]) -> Output {
        let mut c = common::zam_cmd();
        c.env("ZAM_URL", &self.url)
            .env("ZAM_TOKEN", &self.token)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .current_dir(self.repo.join(cwd))
            .args(args)
            .write_stdin(stdin.to_vec())
            .output()
            .unwrap()
    }

    fn zam(&self, args: &[&str]) -> Output {
        self.zam_in("", args, b"text\n")
    }

    fn get(&self, uri: &str) -> Value {
        let r = http()
            .get(&format!("{}/api/v1{uri}", self.url))
            .header("Authorization", format!("Bearer {}", self.token))
            .call()
            .unwrap();
        serde_json::from_str(&r.into_body().read_to_string().unwrap()).unwrap()
    }

    fn index(&self) -> Value {
        self.get(&format!("/file-refs?solution={G}"))
    }
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

fn id_of(s: &str) -> String {
    s.trim().split('@').next().unwrap().to_string()
}

fn blake(b: &[u8]) -> String {
    blake3::hash(b).to_hex().to_string()
}

/// Элемент индекса пути или `Null`.
fn path_of(idx: &Value, path: &str) -> Value {
    idx["refs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["path"] == path)
        .cloned()
        .unwrap_or(Value::Null)
}

fn states(idx: &Value, path: &str) -> Vec<String> {
    path_of(idx, path)["items"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|i| i["state"].as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// `git` в каталоге: код 0 обязателен. Нет `git` — `None` (тест пропускается).
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let o = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=zam",
            "-c",
            "user.email=zam@example.invalid",
        ])
        .args(["-c", "commit.gpgsign=false", "-c", "core.autocrlf=false"])
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .ok()?;
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// `zam log changelog --ref <путь>` и `zam log insight --ref`: в запросе `refs.files` —
/// нормализованный путь, blake3 содержимого и `git rev-parse HEAD`; в `data.refs.files` — тот
/// же путь; прочие виды `--ref` — как раньше.
#[test]
fn log_ref_sends_hash_and_commit() {
    let (url, seen) = stub();
    let p = Proj::new("log-ref", &url, "zam_stub");
    p.file("src/a.rs", b"fn a() {}\n");
    let head = git(&p.repo, &["init", "-q"]).and_then(|_| {
        git(&p.repo, &["add", "src/a.rs"])?;
        git(&p.repo, &["commit", "-q", "-m", "init"])?;
        git(&p.repo, &["rev-parse", "HEAD"])
    });
    let o = p.zam_in(
        "src",
        &[
            "log",
            "changelog",
            "--kind",
            "fix",
            "--title",
            "Правка a",
            "--ref",
            "a.rs",
            "--ref",
            "commit:abc1234",
            "--ref",
            "file:./a.rs",
        ],
        b"text\n",
    );
    ok(&o);
    let s = seen.lock().unwrap().clone();
    let (_, path, body) = s
        .iter()
        .find(|(m, p, _)| m == "POST" && p.ends_with("/entries"))
        .expect("POST записи");
    assert_eq!(path, &format!("/api/v1/spaces/{G}/entries"));
    let mut want = json!({"path": "src/a.rs", "hash": blake(b"fn a() {}\n")});
    if let Some(h) = &head {
        want["commit"] = json!(h);
    }
    assert_eq!(body["refs"], json!({"files": [want]}), "{body}");
    assert_eq!(body["data"]["refs"]["files"], json!(["src/a.rs"]));
    assert_eq!(body["data"]["refs"]["commits"], json!(["abc1234"]));
    // insight: --ref повторяемый, файла нет — ссылка без хэша (предупреждение в stderr)
    let o = p.zam(&[
        "log",
        "insight",
        "--kind",
        "gotcha",
        "--title",
        "Ловушка",
        "--ref",
        "src/a.rs",
        "--ref",
        "docs/нет.md",
    ]);
    ok(&o);
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("docs/нет.md: файла нет"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let s = seen.lock().unwrap().clone();
    let body = &s
        .iter()
        .rev()
        .find(|(m, p, _)| m == "POST" && p.ends_with("/entries"))
        .unwrap()
        .2;
    let files = body["refs"]["files"].as_array().unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0]["hash"], blake(b"fn a() {}\n"));
    assert_eq!(files[1], json!({"path": "docs/нет.md"}));
    // путь выше корня — код 6, запроса нет
    let before = seen.lock().unwrap().len();
    let o = p.zam(&[
        "log",
        "changelog",
        "--kind",
        "fix",
        "--title",
        "x",
        "--ref",
        "../x.rs",
    ]);
    assert_eq!(o.status.code(), Some(6), "{}", out(&o));
    assert_eq!(seen.lock().unwrap().len(), before);
}

/// `zam refs check`: пути ссылок решения сверяются с рабочей копией, изменившиеся уходят
/// `POST /files/changed`; `zam refs verify` возвращает `ok`; `zam write --ref` дописывает
/// `refs:` во frontmatter и подтверждает хэш; `zam links file:` — что известно о файле.
#[test]
fn refs_check_reports_changed() {
    let (url, tok) = start("check");
    let p = Proj::new("check", &url, &tok);
    p.file("src/a.rs", b"a1\n");
    p.file("src/b.rs", b"b1\n");
    let id = id_of(&ok(&p.zam(&[
        "log",
        "insight",
        "--kind",
        "gotcha",
        "--title",
        "Про a и b",
        "--ref",
        "src/a.rs",
        "--ref",
        "src/b.rs",
    ])));
    let idx = p.index();
    assert_eq!(path_of(&idx, "src/a.rs")["hash"], blake(b"a1\n"));
    let o = ok(&p.zam(&["refs", "check"]));
    assert_eq!(o.trim(), "проверено 2, изменилось 0");
    p.file("src/a.rs", b"a2\n");
    let o = ok(&p.zam(&["refs", "check"]));
    assert!(
        o.starts_with("проверено 2, изменилось 1 (устаревших ссылок: 1)"),
        "{o}"
    );
    assert!(o.contains("  src/a.rs"), "{o}");
    let idx = p.index();
    assert_eq!(states(&idx, "src/a.rs"), ["stale"]);
    assert_eq!(states(&idx, "src/b.rs"), ["ok"]);
    let j: Value = serde_json::from_str(&ok(&p.zam(&["refs", "check", "--json"]))).unwrap();
    assert_eq!(j["checked"], 2);
    assert_eq!(j["paths"], json!(["src/a.rs"]));
    // подтверждение: снова ok с новым хэшем
    let o = ok(&p.zam(&["refs", "verify", "src/a.rs"]));
    assert_eq!(o.trim(), "подтверждено ссылок: 1");
    let idx = p.index();
    assert_eq!(states(&idx, "src/a.rs"), ["ok"]);
    assert_eq!(path_of(&idx, "src/a.rs")["hash"], blake(b"a2\n"));
    assert_eq!(
        ok(&p.zam(&["refs", "check"])).trim(),
        "проверено 2, изменилось 0"
    );
    // удалённый файл — изменился
    std::fs::remove_file(p.repo.join("src/b.rs")).unwrap();
    assert!(ok(&p.zam(&["refs", "check"])).starts_with("проверено 2, изменилось 1"));
    assert_eq!(states(&p.index(), "src/b.rs"), ["stale"]);
    // заметка: refs во frontmatter, хэш — по содержимому
    p.file("src/c.rs", b"c1\n");
    let o = p.zam_in(
        "",
        &["write", "arch/pool.md", "--ref", "src/c.rs"],
        "---\ntags: [пул]\n---\n# Пул\nтекст\n".as_bytes(),
    );
    ok(&o);
    let note = p.get(&format!(
        "/nodes/by-path?space={G}&path=arch/pool.md&format=json"
    ));
    let content = note["content"].as_str().unwrap();
    assert!(content.contains("refs:\n  - \"src/c.rs\"\n"), "{content}");
    assert!(content.contains("tags: [пул]"), "{content}");
    let idx = p.index();
    assert_eq!(path_of(&idx, "src/c.rs")["hash"], blake(b"c1\n"), "{idx}");
    assert_eq!(path_of(&idx, "src/c.rs")["items"][0]["kind"], "note");
    // links file: — владельцы ссылок
    let o = ok(&p.zam_in("src", &["links", "file:a.rs"], b""));
    assert!(o.contains(&format!("e:{id}\tПро a и b")), "{o}");
}

/// `zam refs check --since <commit>`: набор сужают `git diff --name-only C..HEAD` и
/// `git status --porcelain` — неизменённый с коммита файл не проверяется.
#[test]
fn refs_check_since_uses_git_diff() {
    let (url, tok) = start("since");
    let p = Proj::new("since", &url, &tok);
    p.file("src/a.rs", b"a1\n");
    p.file("src/b.rs", b"b1\n");
    p.file("src/c.rs", b"c1\n");
    if git(&p.repo, &["init", "-q"]).is_none() {
        eprintln!("git не найден — тест пропущен");
        return;
    }
    git(&p.repo, &["add", "src"]);
    git(&p.repo, &["commit", "-q", "-m", "init"]);
    let c1 = git(&p.repo, &["rev-parse", "HEAD"]).unwrap();
    ok(&p.zam(&[
        "log",
        "insight",
        "--kind",
        "gotcha",
        "--title",
        "Про три файла",
        "--ref",
        "src/a.rs",
        "--ref",
        "src/b.rs",
        "--ref",
        "src/c.rs",
    ]));
    // b — в коммите после c1, c — в рабочей копии; a не трогали
    p.file("src/b.rs", b"b2\n");
    git(&p.repo, &["commit", "-q", "-am", "b"]);
    p.file("src/c.rs", b"c2\n");
    let o = ok(&p.zam(&["refs", "check", "--since", &c1]));
    assert!(o.starts_with("проверено 2, изменилось 2"), "{o}");
    let idx = p.index();
    assert_eq!(states(&idx, "src/a.rs"), ["ok"]);
    assert_eq!(states(&idx, "src/b.rs"), ["stale"]);
    assert_eq!(states(&idx, "src/c.rs"), ["stale"]);
    // после HEAD изменён только c (в рабочей копии): он один и проверяется — по-прежнему
    // отличается от записанного хэша
    let head = git(&p.repo, &["rev-parse", "HEAD"]).unwrap();
    let o = ok(&p.zam(&["refs", "check", "--since", &head]));
    assert!(o.starts_with("проверено 1, изменилось 1"), "{o}");
    // неверный коммит — ошибка, а не «проверено 0»
    let o = p.zam(&["refs", "check", "--since", "нет-такого"]);
    assert_ne!(o.status.code(), Some(0));
    let o = p.zam(&["refs", "check", "--since", "--all"]);
    assert_eq!(o.status.code(), Some(6));
}

/// Review Focus 4: `\` и другой регистр на Windows, абсолютный путь внутри корня — тот же путь
/// ссылки, в регистре файловой системы; вне корня, выше корня, имя устройства, поток NTFS —
/// код 6.
#[test]
fn refs_paths_windows_case_and_backslash() {
    let (url, tok) = start("paths");
    let p = Proj::new("paths", &url, &tok);
    p.file("Src/Main.rs", b"fn main() {}\n");
    let given = if cfg!(windows) {
        "src\\MAIN.rs"
    } else {
        "Src\\Main.rs"
    };
    let id = id_of(&ok(&p.zam(&[
        "log",
        "changelog",
        "--kind",
        "fix",
        "--title",
        "Регистр",
        "--ref",
        given,
    ])));
    let idx = p.index();
    let r = path_of(&idx, "Src/Main.rs");
    assert_eq!(r["hash"], blake(b"fn main() {}\n"), "{idx}");
    let e = p.get(&format!("/entries/{id}"));
    assert_eq!(e["data"]["refs"]["files"], json!(["Src/Main.rs"]), "{e}");
    // абсолютный путь внутри корня и путь из подкаталога — тот же
    let abs = p.repo.join("Src").join("Main.rs");
    let id2 = id_of(&ok(&p.zam(&[
        "log",
        "insight",
        "--kind",
        "gotcha",
        "--title",
        "Абсолютный",
        "--ref",
        abs.to_str().unwrap(),
    ])));
    let rel_sub = if cfg!(windows) { "main.rs." } else { "Main.rs" };
    let id3 = id_of(&ok(&p.zam_in(
        "Src",
        &[
            "log",
            "insight",
            "--kind",
            "gotcha",
            "--title",
            "Из подкаталога",
            "--ref",
            rel_sub,
        ],
        b"text\n",
    )));
    let idx = p.index();
    let main = path_of(&idx, "Src/Main.rs");
    let ids: Vec<&str> = main["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    for want in [format!("c:{id}"), format!("i:{id2}"), format!("i:{id3}")] {
        assert!(ids.contains(&want.as_str()), "{want}: {idx}");
    }
    assert_eq!(idx["refs"].as_array().unwrap().len(), 1, "один путь: {idx}");
    // отказы — код 6 до запроса
    let outside = p.repo.parent().unwrap().join("x.rs");
    let mut bad = vec![
        "../x.rs".to_string(),
        outside.to_string_lossy().into_owned(),
    ];
    if cfg!(windows) {
        bad.extend([
            "CON".into(),
            "src/nul.txt".into(),
            "Src/Main.rs:secret".into(),
        ]);
    }
    for b in &bad {
        let o = p.zam(&[
            "log",
            "changelog",
            "--kind",
            "fix",
            "--title",
            "x",
            "--ref",
            b,
        ]);
        assert_eq!(o.status.code(), Some(6), "{b}: {}", out(&o));
    }
    // verify — без учёта регистра на Windows
    if cfg!(windows) {
        let o = ok(&p.zam(&["refs", "verify", "SRC\\main.RS"]));
        assert_eq!(o.trim(), "подтверждено ссылок: 3");
    }
}
