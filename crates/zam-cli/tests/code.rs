//! `zam code …` (M5.6 задача 5, спецификация §4, §5, §9 CLI, §12, §13, §17 п. 2, 7, 9):
//! отпечатки по git и обходу, `code-state.json`, двухшаговая синхронизация пачками, журнал
//! падений, замок, конфигурация `[code]`, просмотр карты, символов и влияния, коды выхода.
//!
//! Сервер — настоящий `zam-server` в процессе тестов (без моделей); тело запросов проверяется
//! заглушкой `common::stub`. Проект — каталог `common::sandbox` (маркер `.git` уровнем выше не
//! пускает корень и git к репозиторию ZeroAIMemory), git-репозиторий — внутри него.
mod common;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use zam_server::build;
use zam_server::config::ServerConfig;
use zam_store::Access;

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e71";

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
    let data = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("cli-code-{name}-data"));
    let _ = std::fs::remove_dir_all(&data);
    std::fs::create_dir_all(&data).unwrap();
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

/// `git` в каталоге: код 0 обязателен. Нет `git` — `None`.
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
        .env_remove("GIT_INDEX_FILE")
        .output()
        .ok()?;
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn have_git() -> bool {
    std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

struct Proj {
    url: String,
    token: String,
    repo: PathBuf,
    home: PathBuf,
}

impl Proj {
    /// Проект без git: `sandbox/w` с `solution.toml`, «домашний» каталог рядом.
    fn plain(name: &str, url: &str, token: &str) -> Proj {
        let repo = common::sandbox("cli-code", name);
        let home = repo.parent().unwrap().join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(repo.join(".zeroaimemory")).unwrap();
        std::fs::write(
            repo.join(".zeroaimemory/solution.toml"),
            format!("agent_solution_id = \"{G}\"\nname = \"Карта\"\n"),
        )
        .unwrap();
        Proj {
            url: url.into(),
            token: token.into(),
            repo,
            home,
        }
    }

    /// Проект — git-репозиторий (`.zeroaimemory/` в `.gitignore`); `None` — нет git.
    fn git(name: &str, url: &str, token: &str) -> Option<Proj> {
        if !have_git() {
            eprintln!("SKIP {name}: git не найден");
            return None;
        }
        let p = Proj::plain(name, url, token);
        git(&p.repo, &["init", "-q"])?;
        p.file(".gitignore", b".zeroaimemory/\n");
        Some(p)
    }

    fn file(&self, rel: &str, body: &[u8]) {
        let p = self.repo.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    fn commit(&self, msg: &str) -> String {
        git(&self.repo, &["add", "-A"]).unwrap();
        git(&self.repo, &["commit", "-q", "-m", msg]).unwrap();
        git(&self.repo, &["rev-parse", "HEAD"]).unwrap()
    }

    fn config(&self, toml: &str) {
        self.file(".zeroaimemory/config.toml", toml.as_bytes());
    }

    fn cache(&self, name: &str) -> PathBuf {
        self.repo.join(".zeroaimemory/cache").join(name)
    }

    fn zam_env(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut c = common::zam_cmd();
        c.env("ZAM_URL", &self.url)
            .env("ZAM_TOKEN", &self.token)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .current_dir(&self.repo)
            .args(args);
        for (k, v) in env {
            c.env(k, v);
        }
        c.output().unwrap()
    }

    fn zam(&self, args: &[&str]) -> Output {
        self.zam_env(args, &[])
    }

    /// `zam code sync --json` (код 0) → отчёт.
    fn sync(&self) -> Value {
        json_of(&ok(&self.zam(&["code", "sync", "--json"])))
    }

    fn get(&self, uri: &str) -> Value {
        let r = http()
            .get(&format!("{}/api/v1{uri}", self.url))
            .header("Authorization", format!("Bearer {}", self.token))
            .call()
            .unwrap();
        serde_json::from_str(&r.into_body().read_to_string().unwrap()).unwrap()
    }

    fn status(&self) -> Value {
        self.get(&format!("/code/status?solution={G}"))
    }

    /// Пути символов с именем `q` в карте сервера.
    fn symbol_paths(&self, q: &str) -> Vec<String> {
        let v = self.get(&format!("/code/symbols?solution={G}&q={q}&limit=100"));
        let mut out: Vec<String> = v["symbols"]
            .as_array()
            .unwrap_or_else(|| panic!("{v}"))
            .iter()
            .filter(|s| s["name"] == q)
            .map(|s| s["path"].as_str().unwrap().to_string())
            .collect();
        out.sort();
        out
    }
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

fn json_of(s: &str) -> Value {
    serde_json::from_str(s).unwrap_or_else(|e| panic!("{e}: {s}"))
}

fn blake(b: &[u8]) -> String {
    blake3::hash(b).to_hex().to_string()
}

fn now_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// Причины пропуска карты сервера: путь → причина.
fn skipped(st: &Value) -> Vec<(String, String)> {
    st["skipped"]
        .as_array()
        .unwrap_or_else(|| panic!("{st}"))
        .iter()
        .map(|s| {
            (
                s["path"].as_str().unwrap().to_string(),
                s["reason"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn reason_of(st: &Value, path: &str) -> Option<String> {
    skipped(st)
        .into_iter()
        .find(|(p, _)| p == path)
        .map(|(_, r)| r)
}

/// Три файла Rust: `Store` с методом `open`, вызывающий его `b.rs`, `c.rs` с `helper`.
fn three_files(p: &Proj) {
    p.file(
        "src/a.rs",
        b"pub struct Store;\n\nimpl Store {\n    pub fn open() -> Store {\n        Store\n    }\n}\n",
    );
    p.file(
        "src/b.rs",
        b"use crate::a::Store;\n\npub fn run() {\n    let _s = Store::open();\n}\n",
    );
    p.file("src/c.rs", b"pub fn helper() -> u32 {\n    1\n}\n");
}

/// §17 п. 2: первая синхронизация загружает все файлы, повтор без изменений — ноль; рядом с
/// кодом — состояние `code-state.json` и малый `code-sync.json`.
#[test]
fn code_sync_first_uploads_all_second_zero() {
    let (url, tok) = start("first");
    let Some(p) = Proj::git("first", &url, &tok) else {
        return;
    };
    three_files(&p);
    p.file("README.md", b"# not code\n");
    let head = p.commit("init");
    let r = p.sync();
    assert_eq!(r["files"], 3, "{r}");
    assert_eq!(r["need"], 3, "{r}");
    assert_eq!(r["uploaded"], 3, "{r}");
    let st = p.status();
    assert_eq!(st["files"], 3, "{st}");
    assert_eq!(st["commit"], json!(head), "{st}");
    assert_eq!(p.symbol_paths("open"), ["src/a.rs"]);
    let state = json_of(&std::fs::read_to_string(p.cache("code-state.json")).unwrap());
    assert_eq!(state["solution"], G, "{state}");
    let a = &state["files"]["src/a.rs"];
    assert!(a["fp"].as_str().unwrap().starts_with("oid:"), "{state}");
    assert_eq!(
        a["hash"],
        blake(&std::fs::read(p.repo.join("src/a.rs")).unwrap())
    );
    let mark = json_of(&std::fs::read_to_string(p.cache("code-sync.json")).unwrap());
    assert_eq!(mark["head"], json!(head), "{mark}");
    assert_eq!(mark["solution"], G);
    assert!(
        mark["last_ok_at"].as_str().unwrap().ends_with('Z'),
        "{mark}"
    );
    // повтор — ничего не загружено, замок снят
    let r = p.sync();
    assert_eq!(
        (r["need"].clone(), r["uploaded"].clone()),
        (json!(0), json!(0)),
        "{r}"
    );
    drop(hold_lock(&p)); // замок свободен (файл остаётся, блокировку держит ОС)
    // человекочитаемый вывод — одна строка итога
    let o = ok(&p.zam(&["code", "sync"]));
    assert!(o.contains("файлов 3") && o.contains("загружено 0"), "{o}");
}

/// §17 п. 2: правка одного файла (и неотслеживаемый новый файл) — загружены ровно они.
#[test]
fn code_sync_edit_one_uploads_one() {
    let (url, tok) = start("edit");
    let Some(p) = Proj::git("edit", &url, &tok) else {
        return;
    };
    three_files(&p);
    p.commit("init");
    p.sync();
    p.file("src/c.rs", b"pub fn helper() -> u32 {\n    2\n}\n");
    let r = p.sync();
    assert_eq!(
        (r["need"].clone(), r["uploaded"].clone()),
        (json!(1), json!(1)),
        "{r}"
    );
    let state = json_of(&std::fs::read_to_string(p.cache("code-state.json")).unwrap());
    assert!(
        state["files"]["src/c.rs"]["fp"]
            .as_str()
            .unwrap()
            .starts_with("mt:"),
        "изменённый файл — отпечаток по размеру и mtime: {state}"
    );
    p.file("src/d.py", b"def fresh():\n    return 1\n");
    let r = p.sync();
    assert_eq!(
        (r["files"].clone(), r["uploaded"].clone()),
        (json!(4), json!(1)),
        "{r}"
    );
    assert_eq!(p.symbol_paths("fresh"), ["src/d.py"]);
    // --full — разобраны и загружены все
    let r = json_of(&ok(&p.zam(&["code", "sync", "--full", "--json"])));
    assert_eq!(r["uploaded"], 4, "{r}");
}

/// Review Focus 3: `git mv` и `git rm` — старый путь исчезает из карты, новый загружен.
#[test]
fn code_sync_rename_removes_old_adds_new() {
    let (url, tok) = start("rename");
    let Some(p) = Proj::git("rename", &url, &tok) else {
        return;
    };
    three_files(&p);
    p.commit("init");
    p.sync();
    git(&p.repo, &["mv", "src/c.rs", "src/moved.rs"]).unwrap();
    let r = p.sync();
    assert_eq!(r["removed"], 1, "{r}");
    assert_eq!(r["uploaded"], 1, "{r}");
    assert_eq!(p.symbol_paths("helper"), ["src/moved.rs"]);
    p.commit("mv");
    git(&p.repo, &["rm", "-q", "src/b.rs"]).unwrap();
    let r = p.sync();
    assert_eq!(
        (r["removed"].clone(), r["uploaded"].clone()),
        (json!(1), json!(0)),
        "{r}"
    );
    assert!(p.symbol_paths("run").is_empty());
    // удаление файла правкой (без git) — тоже исчезает
    std::fs::remove_file(p.repo.join("src/moved.rs")).unwrap();
    let r = p.sync();
    assert_eq!(r["removed"], 1, "{r}");
    assert_eq!(p.status()["files"], 1);
}

/// §17 п. 2: вторая машина (копия рабочей копии, `code-state.json` удалён) читает файлы, но
/// ничего не загружает.
#[test]
fn code_sync_second_machine_reset_state_uploads_zero() {
    let (url, tok) = start("second");
    let Some(p) = Proj::git("second", &url, &tok) else {
        return;
    };
    three_files(&p);
    p.commit("init");
    p.sync();
    let q = Proj::plain("second-copy", &url, &tok);
    copy_dir(&p.repo, &q.repo);
    std::fs::remove_file(q.cache("code-state.json")).unwrap();
    let r = q.sync();
    assert_eq!(r["files"], 3, "{r}");
    assert_eq!(
        (r["need"].clone(), r["uploaded"].clone()),
        (json!(0), json!(0)),
        "{r}"
    );
    assert!(q.cache("code-state.json").is_file());
}

fn copy_dir(from: &Path, to: &Path) {
    let mut stack = vec![(from.to_path_buf(), to.to_path_buf())];
    while let Some((src, dst)) = stack.pop() {
        std::fs::create_dir_all(&dst).unwrap();
        for e in std::fs::read_dir(&src).unwrap() {
            let e = e.unwrap();
            let t = e.file_type().unwrap();
            if t.is_dir() {
                stack.push((e.path(), dst.join(e.file_name())));
            } else if t.is_file() {
                std::fs::copy(e.path(), dst.join(e.file_name())).unwrap();
            }
        }
    }
}

/// Символическая ссылка (или точка соединения Windows) на каталог `target` с именем `link`.
fn dir_link(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(target, link).is_ok()
            || std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .is_ok_and(|o| o.status.success())
    }
}

/// §4, §13: вне git — обход без `.git`, `.zeroaimemory`, `target`, `node_modules`, `dist`,
/// `build`, скрытых каталогов и символических ссылок; предфильтр размер+mtime; `exclude`.
#[test]
fn code_sync_outside_git_walk_skips_dirs_and_symlinks() {
    let (url, tok) = start("walk");
    let p = Proj::plain("walk", &url, &tok);
    p.file("src/lib.rs", b"pub fn kept() {}\n");
    p.file("src/deep/x/y/z.py", b"def deep():\n    pass\n");
    for d in [
        "target",
        "node_modules",
        "dist",
        "build",
        ".hidden",
        "src/.cache",
    ] {
        p.file(&format!("{d}/skip.rs"), b"pub fn skipped() {}\n");
    }
    p.file("gen/auto.rs", b"pub fn generated() {}\n");
    p.config("[code]\nexclude = [\"gen/**\"]\n");
    let outside = p.repo.parent().unwrap().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("far.rs"), b"pub fn far() {}\n").unwrap();
    let linked = dir_link(&outside, &p.repo.join("src/link"));
    if !linked {
        eprintln!("символическую ссылку создать не удалось — проверка ссылки пропущена");
    }
    let r = p.sync();
    assert_eq!(r["files"], 2, "{r}");
    let state = json_of(&std::fs::read_to_string(p.cache("code-state.json")).unwrap());
    let files: Vec<&String> = state["files"].as_object().unwrap().keys().collect();
    assert_eq!(files, ["src/deep/x/y/z.py", "src/lib.rs"], "{state}");
    assert!(
        state["files"]["src/lib.rs"]["fp"]
            .as_str()
            .unwrap()
            .starts_with("mt:"),
        "{state}"
    );
    assert!(p.symbol_paths("far").is_empty());
    assert!(p.symbol_paths("skipped").is_empty());
    // повтор: отпечаток тот же — ноль загрузок; правка — одна
    assert_eq!(p.sync()["uploaded"], 0);
    std::thread::sleep(Duration::from_millis(20));
    p.file("src/lib.rs", b"pub fn kept() {}\npub fn more() {}\n");
    assert_eq!(p.sync()["uploaded"], 1);
}

/// Review Focus 5: путь в манифесте — с регистром файловой системы и `/` (в git — индекс
/// хранит другой регистр). Linux: регистр ФС и индекса совпадают — проверка регистра
/// пропускается, `/` проверяется.
#[test]
fn code_fingerprints_real_case_and_forward_slash() {
    let (url, seen) = common::stub(
        200,
        r#"{"need":[],"removed":0,"etag":"e","accepted":0,"stale_refs":0,"warnings":[]}"#,
        Duration::ZERO,
    );
    let Some(p) = Proj::git("case", &url, "zam_stub") else {
        return;
    };
    p.file("src/store.rs", b"pub fn s() {}\n");
    p.file("src/sub/inner.rs", b"pub fn i() {}\n");
    p.commit("init");
    let want_store = if cfg!(windows) {
        // регистр на диске меняется, индекс git (core.ignorecase) — прежний
        std::fs::rename(p.repo.join("src"), p.repo.join("Src_tmp")).unwrap();
        std::fs::rename(p.repo.join("Src_tmp"), p.repo.join("Src")).unwrap();
        std::fs::rename(p.repo.join("Src/store.rs"), p.repo.join("Src/Store.RS")).unwrap();
        "Src/Store.RS"
    } else {
        eprintln!("Linux: регистр ФС совпадает с индексом — проверяется только `/`");
        "src/store.rs"
    };
    ok(&p.zam(&["code", "sync", "--json"]));
    let s = seen.lock().unwrap().clone();
    let (_, _, body) = s
        .iter()
        .find(|(m, p, _)| m == "POST" && p == "/api/v1/code/sync")
        .expect("POST /code/sync");
    let mut paths: Vec<String> = body["manifest"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["path"].as_str().unwrap().to_string())
        .collect();
    paths.sort();
    let inner = if cfg!(windows) {
        "Src/sub/inner.rs"
    } else {
        "src/sub/inner.rs"
    };
    let mut want = vec![want_store.to_string(), inner.to_string()];
    want.sort();
    assert_eq!(paths, want, "{body}");
    assert!(paths.iter().all(|p| !p.contains('\\')));
}

/// §17 п. 7: враждебные файлы — синхронизация завершается, причины пропуска верные.
#[test]
fn code_sync_hostile_fixtures_reasons() {
    let (url, tok) = start("hostile");
    let p = Proj::plain("hostile", &url, &tok);
    p.config("[code]\nmax_file_bytes = 16777216\nparse_timeout_ms = 10\n");
    // 10 МБ минифицированного JS — одна длинная строка
    let mut js = Vec::with_capacity(10 * 1024 * 1024);
    while js.len() < 10 * 1024 * 1024 {
        js.extend_from_slice(b"var a=function(b){return b+1};");
    }
    p.file("min.js", &js);
    p.file("nest.rs", "(\n".repeat(100_000).as_bytes());
    p.file("bin.rs", b"fn a() {}\0\0\x01\x02 binary\n");
    p.file("bad.rs", b"pub fn valid() {}\n// \xff\xfe invalid utf-8\n");
    let mut slow = String::with_capacity(5 * 1024 * 1024);
    let mut i = 0u32;
    while slow.len() < 4 * 1024 * 1024 {
        slow.push_str(&format!(
            "pub fn function_{i}(x: u32) -> u32 {{ let y = x + {i}; y * 2 }}\n"
        ));
        i += 1;
    }
    p.file("slow.rs", slow.as_bytes());
    let r = p.sync();
    assert_eq!(r["files"], 5, "{r}");
    let st = p.status();
    assert_eq!(
        reason_of(&st, "min.js").as_deref(),
        Some("minified"),
        "{st}"
    );
    assert_eq!(reason_of(&st, "bin.rs").as_deref(), Some("binary"), "{st}");
    assert_eq!(reason_of(&st, "bad.rs"), None, "{st}");
    assert_eq!(
        reason_of(&st, "slow.rs").as_deref(),
        Some("timeout"),
        "{st}"
    );
    assert_eq!(p.symbol_paths("valid"), ["bad.rs"]);
    assert!(r["skipped"]["minified"].as_u64() == Some(1), "{r}");
}

/// §13: незакрытая запись журнала падений → файл `parser_crash`, пока хэш тот же; после
/// правки — разбирается. Обрыв процесса на файле (крючок debug-сборки) оставляет запись.
#[test]
fn code_sync_crash_journal_skips_parser_crash() {
    let (url, tok) = start("crash");
    let p = Proj::plain("crash", &url, &tok);
    three_files(&p);
    let a = std::fs::read(p.repo.join("src/a.rs")).unwrap();
    p.file(
        ".zeroaimemory/cache/code-parsing.json",
        json!({"path": "src/a.rs", "hash": blake(&a)})
            .to_string()
            .as_bytes(),
    );
    let r = p.sync();
    assert_eq!(r["skipped"]["parser_crash"], 1, "{r}");
    assert!(!p.cache("code-parsing.json").exists());
    let st = p.status();
    assert_eq!(reason_of(&st, "src/a.rs").as_deref(), Some("parser_crash"));
    // --full: пропуск держится, пока хэш тот же
    let r = json_of(&ok(&p.zam(&["code", "sync", "--full", "--json"])));
    assert_eq!(r["skipped"]["parser_crash"], 1, "{r}");
    std::thread::sleep(Duration::from_millis(20));
    p.file("src/a.rs", b"pub struct Store;\npub fn fixed() {}\n");
    let r = p.sync();
    assert_eq!(r["uploaded"], 1, "{r}");
    assert_eq!(reason_of(&p.status(), "src/a.rs"), None);
    assert_eq!(p.symbol_paths("fixed"), ["src/a.rs"]);
    // обрыв процесса во время разбора (только debug-сборка): два файла подряд — третий запуск
    // пропускает оба (ревью задачи 5, I1: запись первого обрыва не теряется вторым)
    if cfg!(debug_assertions) {
        std::thread::sleep(Duration::from_millis(20));
        p.file("src/c.rs", b"pub fn helper() -> u32 {\n    3\n}\n");
        p.file("src/b.rs", b"pub fn run() {}\n");
        for victim in ["src/b.rs", "src/c.rs"] {
            let o = p.zam_env(
                &["code", "sync", "--json"],
                &[("ZAM_DEBUG_CODE_ABORT", victim)],
            );
            assert_ne!(o.status.code(), Some(0), "{victim}: {}", err(&o));
            let j = json_of(&std::fs::read_to_string(p.cache("code-parsing.json")).unwrap());
            assert_eq!(j["path"], victim, "{j}");
        }
        // замок обрыва держала ОС — он снят; оба файла — parser_crash
        let r = p.sync();
        assert_eq!(r["skipped"]["parser_crash"], 2, "{r}");
        let st = p.status();
        for victim in ["src/b.rs", "src/c.rs"] {
            assert_eq!(
                reason_of(&st, victim).as_deref(),
                Some("parser_crash"),
                "{st}"
            );
        }
        // и при следующем запуске — тоже (хэши те же)
        let r = json_of(&ok(&p.zam(&["code", "sync", "--full", "--json"])));
        assert_eq!(r["skipped"]["parser_crash"], 2, "{r}");
    }
}

/// Держит замок синхронизации проекта, пока жив (как идущая синхронизация).
fn hold_lock(p: &Proj) -> std::fs::File {
    let lock = p.cache("code-sync.lock");
    std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock)
        .unwrap();
    f.try_lock().unwrap();
    f
}

/// Review Focus 4: замок — блокировка файла ОС. Занят — вторая синхронизация не стартует (код
/// 0, строка в stderr; `--background` — строка в `hooks.log`); снят — синхронизация идёт;
/// ошибка и завершение процесса замок снимают. Два настоящих процесса одновременно — второй
/// не стартует.
#[test]
fn code_sync_lock_second_skips_stale_lock_taken() {
    let (url, tok) = start("lock");
    let p = Proj::plain("lock", &url, &tok);
    three_files(&p);
    let held = hold_lock(&p);
    let o = p.zam(&["code", "sync"]);
    ok(&o);
    assert!(err(&o).contains("синхронизация уже идёт"), "{}", err(&o));
    assert!(!p.cache("code-state.json").exists(), "синхронизация не шла");
    let o = p.zam(&["code", "sync", "--background"]);
    assert_eq!(o.status.code(), Some(0));
    assert!(out(&o).is_empty() && err(&o).is_empty(), "{}", err(&o));
    drop(held);
    // файл замка остался (старое содержимое `{pid, at}` ни на что не влияет)
    std::fs::write(
        p.cache("code-sync.lock"),
        json!({"pid": 1, "at": now_s()}).to_string(),
    )
    .unwrap();
    let r = p.sync();
    assert_eq!(r["uploaded"], 3, "{r}");
    // ошибка синхронизации (сервер недоступен) замок снимает
    let o = p.zam_env(&["code", "sync"], &[("ZAM_URL", "http://127.0.0.1:9")]);
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    let o = p.zam(&["code", "sync", "--background"]);
    assert_eq!(o.status.code(), Some(0));
    assert!(out(&o).is_empty(), "{}", out(&o));
    let log = std::fs::read_to_string(p.repo.join(".zeroaimemory/logs/hooks.log")).unwrap();
    let lines: Vec<&str> = log.lines().filter(|l| l.contains("code-sync")).collect();
    assert_eq!(lines.len(), 2, "{log}");
    assert!(lines[0].contains("синхронизация уже идёт"), "{log}");
    assert!(lines[1].contains("файлов 3"), "{log}");
    // два настоящих процесса: первый держит замок, пока ждёт медленную заглушку
    let (slow, seen) = common::stub(
        200,
        r#"{"need":[],"removed":0,"etag":"e","accepted":0,"stale_refs":0,"warnings":[]}"#,
        Duration::from_millis(1500),
    );
    let q = Proj::plain("lock-two", &slow, "zam_stub");
    three_files(&q);
    // фоновый процесс: std-команда с тем же чистым окружением, что у `common::zam_cmd`
    let mut first = std::process::Command::new(assert_cmd::cargo::cargo_bin("zam"));
    for k in common::PROXY_VARS {
        first.env_remove(k);
    }
    for (k, _) in std::env::vars_os() {
        if let Some(k) = k.to_str()
            && (k.starts_with("ZAM_")
                || zam_core::AGENT_ENV_VARS.contains(&k)
                || zam_core::CLIENT_ENV_VARS.contains(&k))
        {
            first.env_remove(k);
        }
    }
    first
        .env("ZAM_URL", &q.url)
        .env("ZAM_TOKEN", &q.token)
        .env("HOME", &q.home)
        .env("USERPROFILE", &q.home)
        .current_dir(&q.repo)
        .args(["code", "sync"]);
    let mut child = first
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let t0 = std::time::Instant::now();
    while seen.lock().unwrap().is_empty() {
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "первый процесс не дошёл до сервера"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let o = q.zam(&["code", "sync"]);
    ok(&o);
    assert!(err(&o).contains("синхронизация уже идёт"), "{}", err(&o));
    assert!(child.wait().unwrap().success());
    // первый завершился — замок свободен
    let o = q.zam(&["code", "sync"]);
    ok(&o);
    assert!(!err(&o).contains("уже идёт"), "{}", err(&o));
}

/// Токен вида GitHub в сигнатуре фикстуры.
const SECRET_TOKEN: &str = "ghp_Ab3dEf6hIj9kLm2nOp5qRs8tUv1wXy4zAb7c";

const STUB_SYNC: &str = r#"{"need":["src/a.rs","src/long.ts"],"removed":0,"etag":"e","accepted":2,"stale_refs":0,"warnings":[]}"#;

/// Тела запросов `zam code sync` (заглушка).
fn bodies(seen: &common::Seen, method: &str, path: &str) -> Vec<Value> {
    seen.lock()
        .unwrap()
        .iter()
        .filter(|(m, p, _)| m == method && p == path)
        .map(|(_, _, b)| b.clone())
        .collect()
}

/// Ключи объекта.
fn keys(v: &Value) -> Vec<String> {
    v.as_object()
        .unwrap_or_else(|| panic!("{v}"))
        .keys()
        .cloned()
        .collect()
}

/// §4, §17 п. 9: на сервер уходят только метаданные: поля §4, без содержимого файлов, `sig`
/// не длиннее 160 символов; `send_signatures = false` — без `sig`.
#[test]
fn code_sync_request_only_metadata() {
    for send_sig in [true, false] {
        let (url, seen) = common::stub(200, STUB_SYNC, Duration::ZERO);
        let Some(p) = Proj::git(&format!("meta-{send_sig}"), &url, "zam_stub") else {
            return;
        };
        p.file(
            "src/a.rs",
            b"pub fn secret_holder() -> &'static str {\n    \"BODY_MARKER_7f3a\"\n}\n\
pub fn one() -> u32 { 1 } static NEIGHBOUR_X: &str = \"NEIGHBOUR_7f3a\";\n",
        );
        let mut long = format!(
            "export function longSignature({}) {{\n  return 'BODY_MARKER_7f3a';\n}}\n",
            (0..60)
                .map(|i| format!("a{i}: number"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        // ревью задачи 4: токен на границе обрезки (символы 147–186) не уходит открытым даже
        // частью — клиент маскирует до обрезки
        long.push_str(&format!(
            "export function withKey({}, key = \"{SECRET_TOKEN}\") {{\n  return key;\n}}\n",
            "b".repeat(116)
        ));
        p.file("src/long.ts", long.as_bytes());
        p.commit("init");
        if !send_sig {
            p.config("[code]\nsend_signatures = false\n");
        }
        ok(&p.zam(&["code", "sync"]));
        let sync = bodies(&seen, "POST", "/api/v1/code/sync");
        assert_eq!(sync.len(), 1);
        let mut k = keys(&sync[0]);
        k.sort();
        let allowed = ["branch", "commit", "machine", "manifest", "solution"];
        assert!(k.iter().all(|x| allowed.contains(&x.as_str())), "{k:?}");
        for m in sync[0]["manifest"].as_array().unwrap() {
            let mut mk = keys(m);
            mk.sort();
            assert_eq!(mk, ["hash", "path"]);
        }
        let puts = bodies(&seen, "PUT", "/api/v1/code/files");
        assert_eq!(puts.len(), 1, "{puts:?}");
        let text = puts[0].to_string();
        assert!(
            !text.contains("BODY_MARKER_7f3a"),
            "содержимое файла ушло: {text}"
        );
        assert!(!text.contains(&SECRET_TOKEN[..8]), "секрет ушёл: {text}");
        // однострочное определение: в сигнатуру `one` соседний код строки не попадает (I2)
        if send_sig {
            let one = puts[0]["files"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|f| f["symbols"].as_array().unwrap().iter())
                .find(|s| s["name"] == "one")
                .cloned()
                .expect("символ one");
            assert_eq!(one["sig"], "pub fn one() -> u32 { 1 }", "{one}");
        }
        let files = puts[0]["files"].as_array().unwrap();
        assert_eq!(files.len(), 2);
        let file_keys = [
            "git_oid",
            "hash",
            "lang",
            "parse_state",
            "path",
            "refs",
            "size",
            "skip_reason",
            "symbols",
        ];
        let sym_keys = [
            "body_hash",
            "end_line",
            "exported",
            "kind",
            "line",
            "name",
            "qual",
            "sig",
        ];
        let mut sigs = 0;
        for f in files {
            assert!(
                keys(f).iter().all(|x| file_keys.contains(&x.as_str())),
                "{f}"
            );
            assert_eq!(f["git_oid"].as_str().map(str::len), Some(40), "{f}");
            for s in f["symbols"].as_array().unwrap() {
                assert!(
                    keys(s).iter().all(|x| sym_keys.contains(&x.as_str())),
                    "{s}"
                );
                if let Some(sig) = s.get("sig") {
                    sigs += 1;
                    assert!(sig.as_str().unwrap().chars().count() <= 160, "{s}");
                }
            }
            for r in f["refs"].as_array().unwrap() {
                let mut rk = keys(r);
                rk.sort();
                assert_eq!(rk, ["count", "first_line", "kind", "name"]);
            }
        }
        if send_sig {
            assert!(sigs >= 2, "{text}");
            assert!(text.contains('…'), "длинная сигнатура обрезана: {text}");
        } else {
            assert_eq!(sigs, 0, "{text}");
        }
    }
}

/// Заглушка, у которой `need` — все пути `paths`.
fn stub_need(paths: &[String]) -> (String, common::Seen) {
    let body = json!({"need": paths, "removed": 0, "etag": "e", "accepted": 1,
                      "stale_refs": 0, "warnings": []})
    .to_string();
    common::stub(200, Box::leak(body.into_boxed_str()), Duration::ZERO)
}

/// §5: пачки `PUT /code/files` — не больше 500 файлов и 8 МиБ JSON.
#[test]
fn code_sync_batches_500_and_8mib() {
    // 1201 маленький файл → 500 + 500 + 201
    let paths: Vec<String> = (0..1201).map(|i| format!("m/f{i:04}.py")).collect();
    let (url, seen) = stub_need(&paths);
    let p = Proj::plain("batch-count", &url, "zam_stub");
    for (i, path) in paths.iter().enumerate() {
        p.file(path, format!("def f{i}():\n    return {i}\n").as_bytes());
    }
    ok(&p.zam(&["code", "sync"]));
    let puts = bodies(&seen, "PUT", "/api/v1/code/files");
    let sizes: Vec<usize> = puts
        .iter()
        .map(|b| b["files"].as_array().unwrap().len())
        .collect();
    assert_eq!(sizes, [500, 500, 201]);
    // 15 файлов по 5000 символов (≈ 1,3 МБ JSON каждый) → пачки ≤ 8 МиБ
    let paths: Vec<String> = (0..15).map(|i| format!("big/b{i:02}.rs")).collect();
    let (url, seen) = stub_need(&paths);
    let p = Proj::plain("batch-bytes", &url, "zam_stub");
    // debug-сборка на медленной машине CI не успевает разобрать 5000 функций за 200 мс по
    // умолчанию — файл ушёл бы без символов, и пачка была бы одна
    p.config(
        "[code]
parse_timeout_ms = 10000
",
    );
    for (i, path) in paths.iter().enumerate() {
        let mut s = String::new();
        for j in 0..5000 {
            s.push_str(&format!(
                "pub fn function_with_a_rather_long_name_{i:02}_{j:05}() {{}}\n"
            ));
        }
        p.file(path, s.as_bytes());
    }
    ok(&p.zam(&["code", "sync"]));
    let puts = bodies(&seen, "PUT", "/api/v1/code/files");
    assert!(puts.len() >= 3, "{}", puts.len());
    let mut total = 0;
    for b in &puts {
        let n = b.to_string().len();
        assert!(n <= 8 * 1024 * 1024, "{n}");
        total += b["files"].as_array().unwrap().len();
    }
    assert_eq!(total, 15);
}

/// §12: диапазоны `[code]` (нарушение — код 1 с именем ключа), проект сильнее домашнего.
#[test]
fn code_config_ranges_and_project_over_home() {
    let (url, seen) = common::stub(
        200,
        r#"{"need":[],"removed":0,"etag":"e","accepted":0,"stale_refs":0,"warnings":[]}"#,
        Duration::ZERO,
    );
    let p = Proj::plain("config", &url, "zam_stub");
    p.file("a.rs", b"fn a() {}\n");
    p.file("b.py", b"def b():\n    pass\n");
    p.file("c.go", b"package c\n");
    for (bad, key) in [
        ("max_file_bytes = 10", "code.max_file_bytes"),
        ("max_file_bytes = 16777217", "code.max_file_bytes"),
        ("max_files = 0", "code.max_files"),
        ("parse_timeout_ms = 9", "code.parse_timeout_ms"),
        ("auto_interval_min = 10081", "code.auto_interval_min"),
        ("languages = [\"cobol\"]", "code.languages"),
        ("exclude = [\"[\"]", "code.exclude"),
        ("unknown_key = 1", "unknown_key"),
    ] {
        p.config(&format!("[code]\n{bad}\n"));
        let o = p.zam(&["code", "sync"]);
        assert_eq!(o.status.code(), Some(1), "{bad}: {}", err(&o));
        assert!(err(&o).contains(key), "{bad}: {}", err(&o));
    }
    assert!(bodies(&seen, "POST", "/api/v1/code/sync").is_empty());
    // домашний: только python и go; проект: только rust и go — действует проект
    std::fs::create_dir_all(p.home.join(".zeroaimemory")).unwrap();
    std::fs::write(
        p.home.join(".zeroaimemory/config.toml"),
        "[code]\nlanguages = [\"python\", \"go\"]\nexclude = [\"c.go\"]\n",
    )
    .unwrap();
    p.config("[code]\nlanguages = [\"rust\", \"go\"]\n");
    ok(&p.zam(&["code", "sync"]));
    let sync = bodies(&seen, "POST", "/api/v1/code/sync");
    let mut paths: Vec<&str> = sync[0]["manifest"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["path"].as_str().unwrap())
        .collect();
    paths.sort();
    // languages — из проекта, exclude — из домашнего (в проекте ключа нет)
    assert_eq!(paths, ["a.rs"]);
    // выключено — синхронизация не идёт, код 0 с пояснением
    p.config("[code]\nenabled = false\n");
    let o = p.zam(&["code", "sync"]);
    ok(&o);
    assert!(err(&o).contains("code.enabled"), "{}", err(&o));
    assert_eq!(bodies(&seen, "POST", "/api/v1/code/sync").len(), 1);
}

/// Проект с картой на настоящем сервере.
fn synced(name: &str) -> Option<(Proj, String)> {
    let (url, tok) = start(name);
    let p = Proj::git(name, &url, &tok)?;
    three_files(&p);
    let c = p.commit("init");
    p.sync();
    Some((p, c))
}

/// `zam code map`: `--focus` (путь или имя), `--task`, `--tokens`, `--json` — ответ сервера.
#[test]
fn code_map_focus_tokens_json() {
    let Some((p, _)) = synced("map") else {
        return;
    };
    let v = json_of(&ok(&p.zam(&[
        "code", "map", "--focus", "src/b.rs", "--tokens", "200", "--json",
    ])));
    assert_eq!(v["format"], "json", "{v}");
    assert!(v["tokens"].as_u64().unwrap() <= 200, "{v}");
    let first: Vec<&str> = v["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert!(first.contains(&"src/b.rs"), "{v}");
    let md = ok(&p.zam(&["code", "map", "--tokens", "500"]));
    assert!(md.contains("src/a.rs"), "{md}");
    // запрос: focus — JSON {paths, idents, text}, бюджет — budget_tokens
    let (url, seen) = common::stub(
        200,
        r#"{"format":"md","text":"карта\n","tokens":1}"#,
        Duration::ZERO,
    );
    let q = Proj::plain("map-stub", &url, "zam_stub");
    q.file("src/a.rs", b"fn a() {}\n");
    let o = ok(&q.zam(&[
        "code",
        "map",
        "--focus",
        "src/a.rs",
        "--focus",
        "Store::open",
        "--focus",
        "main.py",
        "--task",
        "починить открытие",
        "--tokens",
        "300",
    ]));
    assert_eq!(o, "карта\n");
    let s = seen.lock().unwrap().clone();
    let (_, uri, _) = s.iter().find(|(m, _, _)| m == "GET").unwrap();
    let query = uri.split_once('?').unwrap().1;
    let params: Vec<(String, String)> = query
        .split('&')
        .map(|kv| {
            let (k, v) = kv.split_once('=').unwrap();
            (k.to_string(), decode(v))
        })
        .collect();
    let get = |k: &str| {
        params
            .iter()
            .find(|(x, _)| x == k)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    assert_eq!(get("budget_tokens"), "300");
    assert_eq!(get("solution"), G);
    let focus = json_of(&get("focus"));
    assert_eq!(
        focus,
        json!({"paths": ["src/a.rs", "main.py"], "idents": ["Store::open"],
               "text": "починить открытие"})
    );
    // бюджет вне диапазона — ошибка ввода до запроса
    let o = q.zam(&["code", "map", "--tokens", "0"]);
    assert_eq!(o.status.code(), Some(6), "{}", err(&o));
}

/// Percent-декодирование параметра запроса (`+` — пробел).
fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let h = std::str::from_utf8(&b[i + 1..i + 3]).unwrap();
                out.push(u8::from_str_radix(h, 16).unwrap());
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap()
}

/// `zam code sym` — карточка символа (Markdown и JSON), `путь#qual`; `zam code refs` — кто
/// ссылается; нет символа — код 5.
#[test]
fn code_sym_and_refs_cards() {
    let Some((p, _)) = synced("sym") else {
        return;
    };
    let v = json_of(&ok(&p.zam(&["code", "sym", "Store::open", "--json"])));
    assert_eq!(v["symbol"]["qual"], "Store::open", "{v}");
    assert_eq!(v["symbol"]["path"], "src/a.rs");
    let md = ok(&p.zam(&["code", "sym", "src/a.rs#Store::open"]));
    assert!(
        md.contains("Store::open") && md.contains("src/a.rs:4"),
        "{md}"
    );
    assert!(md.contains("pub fn open() -> Store"), "{md}");
    let refs = ok(&p.zam(&["code", "refs", "open"]));
    assert!(refs.contains("src/b.rs"), "{refs}");
    let v = json_of(&ok(&p.zam(&["code", "refs", "Store", "--json"])));
    let callers: Vec<&str> = v["callers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["path"].as_str().unwrap())
        .collect();
    assert_eq!(callers, ["src/b.rs"], "{v}");
    let o = p.zam(&["code", "sym", "NoSuchSymbol"]);
    assert_eq!(o.status.code(), Some(5), "{}", err(&o));
}

/// `zam code impact --since <commit>` и без него (отпечатки против `code-state.json`):
/// изменённые и удалённые символы, потребители.
#[test]
fn code_impact_since_commit() {
    let Some((p, c1)) = synced("impact") else {
        return;
    };
    // тело open изменено и закоммичено, helper удалён в рабочей копии
    p.file(
        "src/a.rs",
        b"pub struct Store;\n\nimpl Store {\n    pub fn open() -> Store {\n        let s = Store;\n        s\n    }\n}\n",
    );
    p.commit("open");
    p.file("src/c.rs", b"pub fn other() -> u32 {\n    1\n}\n");
    let v = json_of(&ok(&p.zam(&["code", "impact", "--since", &c1, "--json"])));
    let quals = |k: &str| -> Vec<String> {
        let mut q: Vec<String> = v[k]
            .as_array()
            .unwrap_or_else(|| panic!("{v}"))
            .iter()
            .map(|s| s["qual"].as_str().unwrap().to_string())
            .collect();
        q.sort();
        q
    };
    assert_eq!(quals("changed"), ["Store::open"], "{v}");
    assert_eq!(quals("removed"), ["helper"], "{v}");
    let consumers: Vec<&str> = v["consumers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["path"].as_str().unwrap())
        .collect();
    assert_eq!(consumers, ["src/b.rs"], "{v}");
    // без --since: файлы, чей отпечаток отличается от code-state.json, — те же два
    let v2 = json_of(&ok(&p.zam(&["code", "impact", "--json"])));
    assert_eq!(v2["changed"], v["changed"], "{v2}");
    assert_eq!(v2["removed"], v["removed"], "{v2}");
    let md = ok(&p.zam(&["code", "impact"]));
    assert!(
        md.contains("Store::open") && md.contains("src/b.rs"),
        "{md}"
    );
    // после синхронизации изменений нет
    p.sync();
    let v3 = json_of(&ok(&p.zam(&["code", "impact", "--json"])));
    assert!(v3["changed"].as_array().unwrap().is_empty(), "{v3}");
    let o = p.zam(&["code", "impact", "--since", "--all"]);
    assert_eq!(o.status.code(), Some(6));
}

/// `zam code status [--json]`.
#[test]
fn code_status_json() {
    let Some((p, c)) = synced("status") else {
        return;
    };
    let v = json_of(&ok(&p.zam(&["code", "status", "--json"])));
    assert_eq!(v["exists"], true, "{v}");
    assert_eq!(v["files"], 3);
    assert_eq!(v["commit"], json!(c));
    let md = ok(&p.zam(&["code", "status"]));
    assert!(md.contains("файлов 3"), "{md}");
    assert!(md.contains("rust"), "{md}");
}

/// Коды выхода ТЗ §13.2: нет решения — 1, сервер недоступен — 2, неверный ввод — 6.
#[test]
fn code_commands_exit_codes() {
    let (url, tok) = start("codes");
    let p = Proj::plain("codes", &url, &tok);
    p.file("a.rs", b"fn a() {}\n");
    std::fs::remove_file(p.repo.join(".zeroaimemory/solution.toml")).unwrap();
    for args in [
        &["code", "sync"][..],
        &["code", "status"],
        &["code", "map"],
        &["code", "sym", "a"],
        &["code", "impact"],
    ] {
        let o = p.zam(args);
        assert_eq!(o.status.code(), Some(1), "{args:?}: {}", err(&o));
    }
    let p = Proj::plain("codes-down", "http://127.0.0.1:9", &tok);
    p.file("a.rs", b"fn a() {}\n");
    for args in [&["code", "sync"][..], &["code", "status"], &["code", "map"]] {
        let o = p.zam(args);
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", err(&o));
    }
    let o = p.zam(&["code", "map", "--tokens", "abc"]);
    assert_eq!(o.status.code(), Some(6));
    let o = p.zam(&["code", "frobnicate"]);
    assert_eq!(o.status.code(), Some(6));
}

/// Ревью задачи 5, I3: защита карты от массового удаления — корень не совпадает с каталогом
/// `solution.toml` (код 1); манифест меньше половины прошлой синхронизации — без `--prune` не
/// отправляется (в фоне — строка в `hooks.log`); сбой `git` в настоящем репозитории — ошибка,
/// а не тихий обход.
#[test]
fn code_sync_guards_root_prune_and_git_failure() {
    let (url, tok) = start("guards");
    let Some(p) = Proj::git("guards", &url, &tok) else {
        return;
    };
    for i in 0..24 {
        p.file(
            &format!("src/m{i:02}.rs"),
            format!("pub fn f{i}() {{}}\n").as_bytes(),
        );
    }
    p.commit("init");
    assert_eq!(p.sync()["uploaded"], 24);
    // подкаталог со своим .zeroaimemory/: корень проекта — он, решение — выше
    std::fs::create_dir_all(p.repo.join("src/.zeroaimemory")).unwrap();
    let mut c = common::zam_cmd();
    let o = c
        .env("ZAM_URL", &p.url)
        .env("ZAM_TOKEN", &p.token)
        .env("HOME", &p.home)
        .env("USERPROFILE", &p.home)
        .current_dir(p.repo.join("src"))
        .args(["code", "sync"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(
        err(&o).contains("не совпадает с каталогом решения"),
        "{}",
        err(&o)
    );
    std::fs::remove_dir_all(p.repo.join("src/.zeroaimemory")).unwrap();
    // 20 из 24 файлов исчезли — предохранитель
    for i in 0..20 {
        git(&p.repo, &["rm", "-q", &format!("src/m{i:02}.rs")]).unwrap();
    }
    let o = p.zam(&["code", "sync"]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(err(&o).contains("--prune"), "{}", err(&o));
    assert_eq!(p.status()["files"], 24, "карта не тронута");
    let o = p.zam(&["code", "sync", "--background"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).is_empty(), "{}", err(&o));
    let log = std::fs::read_to_string(p.repo.join(".zeroaimemory/logs/hooks.log")).unwrap();
    assert!(log.contains("--prune"), "{log}");
    let r = json_of(&ok(&p.zam(&["code", "sync", "--prune", "--json"])));
    assert_eq!(r["removed"], 20, "{r}");
    // битый индекс git — ошибка конфигурации, карта не тронута
    std::fs::write(p.repo.join(".git/index"), b"garbage, not an index").unwrap();
    let o = p.zam(&["code", "sync"]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(err(&o).contains("git"), "{}", err(&o));
    assert_eq!(p.status()["files"], 4);
}
