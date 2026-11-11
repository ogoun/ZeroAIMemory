//! Приёмка M5.6 «Карта кода» (спецификация `docs/specs/2026-11-03-m5.6-code-map.md` §17,
//! задача 9), клиентская часть — настоящий `zam` (бинарник пакета) против настоящего
//! `zam-server` в процессе теста (без моделей):
//!
//! - `acceptance_extractor_quality_report` — п. 1: точность и полнота экстрактора на эталонах
//!   восьми языков (печать метрик), ≥ 0,95;
//! - `acceptance_incremental_sync` — п. 2 функционально: первая синхронизация, повтор — 0, правка
//!   одного файла — 1, удаление — файл исчезает из карты, вторая копия со сброшенным
//!   `code-state.json` — 0 (время — `cargo xtask code-bench`);
//! - `acceptance_hostile_and_crash` — п. 7: враждебные файлы пропущены с верной причиной,
//!   символическая ссылка за пределы корня не обходится; обрыв процесса после записи журнала
//!   падений (тестовый крючок debug-сборки `ZAM_DEBUG_CODE_ABORT=<путь>`) → следующий запуск —
//!   `parser_crash`;
//! - `acceptance_symbol_staleness_via_hook` — п. 4 через хуки: `post-tool` правки кладёт хэши
//!   символов, `prompt` отправляет очередь; знание о файле при чтении называет `foo`;
//! - `acceptance_hook_budgets_with_map` — п. 8: бюджеты хуков с загруженной картой и включённой
//!   автосинхронизацией, строка карты в дайджесте и `### Код` в пакете субагенту;
//! - `acceptance_bodies_never_sent` — п. 9: тела запросов `zam code sync` (заглушка) — только
//!   поля §4, `sig` ≤ 160 символов, `send_signatures = false` — без `sig`; секрет сигнатуры
//!   замаскирован в ответе `/code/symbols`;
//! - `acceptance_old_cache_reads` — п. 12: кэш `file-refs.json` клиента M4.5 (без `symbol`) и
//!   строки очереди без `symbols` читаются новым клиентом.
//!
//! Серверная часть приёмки — `crates/zam-server/tests/m56_acceptance.rs`. Тесты — по одному
//! (`serial`): отсечка разбора и хуки чувствительны к нагрузке соседей.
mod common;

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use serde_json::{Value, json};
use zam_code::{Lang, Limits, ParseState, extract};
use zam_server::build;
use zam_server::config::ServerConfig;
use zam_store::Access;

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9c56";
/// Адрес без сервера (порт discard): хуки без сети.
const DEAD: &str = "http://127.0.0.1:9";
/// Секрет формата токена GitHub — собирается из частей (фикстуры без цельных литералов ключей).
const SECRET: &str = concat!("gh", "p_", "Mq7rT2vX9zB4", "nC6kD1fG8hJ3", "lP5sW0yE2uA7");

fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn http() -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .proxy(None)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(120)))
            .build(),
    )
}

/// Сервер в фоновом потоке: адрес и токен `rw` пользователя «Анна» с решением `G`.
fn start(name: &str) -> (String, String) {
    let (url, tok, _) = start_db(name);
    (url, tok)
}

/// То же и путь базы пользователя «Анна» (`<data>/users/<id>.db`).
fn start_db(name: &str) -> (String, String, PathBuf) {
    let data = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("cli-m56-{name}-data"));
    let _ = std::fs::remove_dir_all(&data);
    std::fs::create_dir_all(&data).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let mut cfg = ServerConfig {
                data_dir: data,
                ..ServerConfig::default()
            };
            cfg.recall.timeout_ms = 30_000;
            let (app, st, _) = build(cfg, Some("pw".into())).await.unwrap();
            let u = st.system.create_user("Анна", false).await.unwrap();
            let (_, t) = st.system.issue_token(u.id, "t", Access::Rw).await.unwrap();
            let db = st.cfg.data_dir.join("users").join(format!("{}.db", u.id));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            tx.send((url, t, db)).unwrap();
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
    });
    let (url, tok, db): (String, String, PathBuf) = rx.recv().unwrap();
    let (code, v) = rest(
        &url,
        &tok,
        "POST",
        "/solutions",
        Some(&json!({"agent_solution_id": G, "name": "Карта кода"})),
    );
    assert_eq!(code, 201, "{v}");
    (url, tok, db)
}

/// `(символ, состояние, причина)` ссылок на `path` в базе пользователя (второе соединение к
/// базе сервера, WAL): причина устарелости в REST индекса не отдаётся.
fn db_refs(db: &Path, path: &str) -> Vec<(Option<String>, String, Option<String>)> {
    let c = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap();
    c.busy_timeout(Duration::from_secs(10)).unwrap();
    let mut st = c
        .prepare(
            "SELECT symbol, state, stale_reason FROM file_refs WHERE path = ?1
             ORDER BY coalesce(symbol, '')",
        )
        .unwrap();
    st.query_map([path], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

/// Запрос к REST `/api/v1{path}`: (код, JSON или `Null`).
fn rest(url: &str, tok: &str, method: &str, path: &str, body: Option<&Value>) -> (u16, Value) {
    let a = http();
    let uri = format!("{url}/api/v1{path}");
    let auth = format!("Bearer {tok}");
    let r = match method {
        "GET" => a.get(&uri).header("Authorization", &auth).call(),
        _ => a
            .post(&uri)
            .header("Authorization", &auth)
            .content_type("application/json")
            .send(body.map(Value::to_string).unwrap_or_default()),
    };
    let mut r = r.unwrap_or_else(|e| panic!("{method} {path}: {e}"));
    let code = r.status().as_u16();
    let text = r.body_mut().read_to_string().unwrap_or_default();
    let v = if text.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path}: не JSON ({e}): {text}"))
    };
    (code, v)
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let o = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "m56")
        .env("GIT_AUTHOR_EMAIL", "m56@localhost")
        .env("GIT_COMMITTER_NAME", "m56")
        .env("GIT_COMMITTER_EMAIL", "m56@localhost")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// Проект агента: `sandbox/w` с `solution.toml` решения `G`, «домашний» каталог рядом
/// (`[code] auto = false`: фоновую синхронизацию хуков приёмка не запускает).
struct Proj {
    url: String,
    token: String,
    repo: PathBuf,
    home: PathBuf,
}

impl Proj {
    fn plain(name: &str, url: &str, token: &str) -> Proj {
        let repo = common::sandbox("cli-m56", name);
        let home = repo.parent().unwrap().join("home");
        std::fs::create_dir_all(home.join(".zeroaimemory")).unwrap();
        std::fs::write(
            home.join(".zeroaimemory/config.toml"),
            "[code]\nauto = false\n",
        )
        .unwrap();
        std::fs::create_dir_all(repo.join(".zeroaimemory")).unwrap();
        std::fs::write(
            repo.join(".zeroaimemory/solution.toml"),
            format!("agent_solution_id = \"{G}\"\nname = \"Карта кода\"\n"),
        )
        .unwrap();
        Proj {
            url: url.into(),
            token: token.into(),
            repo,
            home,
        }
    }

    /// Проект — git-репозиторий; `None` — git не найден (тест пропускается).
    fn git(name: &str, url: &str, token: &str) -> Option<Proj> {
        let p = Proj::plain(name, url, token);
        if git(&p.repo, &["init", "-q"]).is_none() {
            eprintln!("SKIP {name}: git не найден");
            return None;
        }
        p.file(".gitignore", b".zeroaimemory/\n");
        Some(p)
    }

    fn file(&self, rel: &str, body: &[u8]) {
        let p = self.repo.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    fn commit(&self) {
        git(&self.repo, &["add", "-A"]).unwrap();
        git(&self.repo, &["commit", "-q", "-m", "m56"]).unwrap();
    }

    fn config(&self, toml: &str) {
        self.file(".zeroaimemory/config.toml", toml.as_bytes());
    }

    fn cache(&self) -> PathBuf {
        self.repo.join(".zeroaimemory/cache")
    }

    fn zam_with(&self, args: &[&str], stdin: &[u8], env: &[(&str, &str)]) -> Output {
        let mut c = common::zam_cmd();
        c.env("ZAM_URL", &self.url)
            .env("ZAM_TOKEN", &self.token)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .current_dir(&self.repo)
            .args(args)
            .write_stdin(stdin.to_vec());
        for (k, v) in env {
            c.env(k, v);
        }
        c.output().unwrap()
    }

    fn zam(&self, args: &[&str]) -> Output {
        self.zam_with(args, b"", &[])
    }

    fn sync(&self) -> Value {
        json_of(&ok(&self.zam(&["code", "sync", "--json"])))
    }

    fn get(&self, path: &str) -> Value {
        let (code, v) = rest(&self.url, &self.token, "GET", path, None);
        assert_eq!(code, 200, "GET {path}: {v}");
        v
    }

    fn status(&self) -> Value {
        self.get(&format!("/code/status?solution={G}"))
    }

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

    /// `zam hook <kind> --agent claude` с событием `ev` на адрес `url`, с бюджетом продукта.
    fn hook_at(&self, url: &str, kind: &str, ev: &Value) -> Output {
        self.hook_cmd(url, kind, ev).output().unwrap()
    }

    fn hook_cmd(&self, url: &str, kind: &str, ev: &Value) -> assert_cmd::Command {
        let mut c = common::zam_cmd();
        c.env("ZAM_URL", url)
            .env("ZAM_TOKEN", &self.token)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .current_dir(&self.repo)
            .args(["hook", kind, "--agent", "claude"])
            .write_stdin(ev.to_string().into_bytes());
        c
    }

    /// Хук ради содержимого — со
    /// щедрым бюджетом (`ZAM_TEST_HOOK_BUDGET_MS`): критерий приёмки — содержимое, а с бюджетом
    /// продукта отладочная сборка на занятой машине молчит (fail-open); время и бюджеты проверяют
    /// `hooks` и release `hook_budgets` (ревью 17b-http M6). Замер
    /// времени (`acceptance_hook_budgets_with_map`) — [`Proj::hook_at`].
    fn hook(&self, kind: &str, ev: &Value) -> Output {
        self.hook_cmd(&self.url, kind, ev)
            .env("ZAM_TEST_HOOK_BUDGET_MS", "10000")
            .output()
            .unwrap()
    }

    fn ev(&self, event: &str, session: &str, extra: Value) -> Value {
        let mut v = json!({"session_id": session, "transcript_path": self.repo.join("t.jsonl"),
                           "cwd": self.repo, "hook_event_name": event});
        for (k, x) in extra.as_object().unwrap() {
            v[k] = x.clone();
        }
        v
    }

    fn post_ok(&self, session: &str, tool: &str, input: Value) -> Value {
        self.ev(
            "PostToolUse",
            session,
            json!({"tool_name": tool, "tool_use_id": "u9", "tool_input": input,
                   "tool_response": {"success": true}}),
        )
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.repo.join(".zeroaimemory/logs/hooks.log")).unwrap_or_default()
    }

    /// `(символ, состояние, None)` ссылок на `path` индекса `/file-refs` (причины в индексе
    /// нет — [`db_refs`]).
    fn refs_of(&self, path: &str) -> Vec<(Option<String>, String, Option<String>)> {
        let idx = self.get(&format!("/file-refs?solution={G}"));
        let mut v: Vec<_> = idx["refs"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["path"] == path)
            .flat_map(|r| r["items"].as_array().unwrap().clone())
            .map(|i| {
                (
                    i["symbol"].as_str().map(str::to_string),
                    i["state"].as_str().unwrap().to_string(),
                    None::<String>,
                )
            })
            .collect();
        v.sort();
        v
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

/// `additionalContext` вывода хука; пусто — пустая строка.
fn context(o: &Output) -> String {
    let s = ok(o);
    if s.trim().is_empty() {
        return String::new();
    }
    json_of(s.trim())["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn reason_of(st: &Value, path: &str) -> Option<String> {
    st["skipped"]
        .as_array()
        .unwrap_or_else(|| panic!("{st}"))
        .iter()
        .find(|s| s["path"] == path)
        .map(|s| s["reason"].as_str().unwrap().to_string())
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

/// Символическая ссылка (или точка соединения Windows) `link` на каталог `target`.
fn dir_link(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
    #[cfg(windows)]
    {
        // `mklink` разбирает `/…` внутри пути как ключ — полный путь с `\`
        let win = |p: &Path| {
            let full = match (p.parent().map(std::fs::canonicalize), p.file_name()) {
                (Some(Ok(dir)), Some(name)) => dir.join(name),
                _ => p.to_path_buf(),
            };
            let s = full.to_string_lossy().replace('/', "\\");
            s.strip_prefix(r"\\?\").map_or(s.clone(), str::to_string)
        };
        std::os::windows::fs::symlink_dir(target, link).is_ok()
            || std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(win(link))
                .arg(win(target))
                .output()
                .is_ok_and(|o| o.status.success())
    }
}

/// Три файла Rust: `Store::open`, вызывающий его `b.rs`, `c.rs` с `helper`.
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

// ---------- п. 1 ----------

/// §17 п. 1: полнота и точность определений (имя + `qual` + вид + строка) на эталонах каждого
/// языка первой очереди (≥ 40 размеченных определений) — ≥ 0,95; метрики печатаются таблицей.
#[test]
fn acceptance_extractor_quality_report() {
    let root = common::repo_root().join("crates/zam-code/tests/fixtures");
    let lim = Limits {
        parse_timeout: Duration::from_secs(5),
        ..Limits::default()
    };
    let mut rows = Vec::new();
    for (lang, dir, file) in [
        (Lang::Rust, "rust", "sample.rs"),
        (Lang::TypeScript, "typescript", "sample.ts"),
        (Lang::Tsx, "tsx", "sample.tsx"),
        (Lang::JavaScript, "javascript", "sample.js"),
        (Lang::Python, "python", "sample.py"),
        (Lang::Go, "go", "sample.go"),
        (Lang::Java, "java", "sample.java"),
        (Lang::CSharp, "csharp", "sample.cs"),
    ] {
        let src = std::fs::read(root.join(dir).join(file)).unwrap();
        let expected: BTreeSet<String> =
            std::fs::read_to_string(root.join(dir).join("expected.txt"))
                .unwrap()
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect();
        assert!(expected.len() >= 40, "{dir}: эталон меньше 40 определений");
        let x = extract(lang, &src, &lim);
        assert_eq!(x.state, ParseState::Ok, "{dir}: {:?}", x.skip);
        let got: BTreeSet<String> = x
            .symbols
            .iter()
            .map(|s| format!("{} {} {}", s.kind.as_str(), s.qual, s.line))
            .collect();
        let tp = got.intersection(&expected).count() as f64;
        rows.push((
            dir,
            expected.len(),
            got.len(),
            tp / got.len().max(1) as f64,
            tp / expected.len() as f64,
        ));
    }
    println!("| Язык | Эталон | Найдено | Точность | Полнота |");
    println!("|---|---|---|---|---|");
    for (dir, e, g, p, r) in &rows {
        println!("| {dir} | {e} | {g} | {p:.3} | {r:.3} |");
    }
    let low: Vec<_> = rows
        .iter()
        .filter(|(_, _, _, p, r)| *p < 0.95 || *r < 0.95)
        .map(|r| r.0)
        .collect();
    assert!(low.is_empty(), "ниже 0,95: {low:?}");
}

// ---------- п. 2 ----------

/// §17 п. 2 (функционально; время — `cargo xtask code-bench`): первая синхронизация загружает
/// все файлы, повтор — 0, правка одного файла — 1, удалённый файл исчезает из карты, вторая
/// копия рабочего каталога со сброшенным `code-state.json` загружает 0.
#[test]
fn acceptance_incremental_sync() {
    let _g = serial();
    let (url, tok) = start("incr");
    let Some(p) = Proj::git("incr", &url, &tok) else {
        return;
    };
    three_files(&p);
    p.file(
        "src/d.ts",
        b"export function dee(): number {\n  return 4;\n}\n",
    );
    p.file("src/e.py", b"def eee():\n    return 5\n");
    p.commit();
    let r = p.sync();
    assert_eq!(
        (r["files"].clone(), r["uploaded"].clone()),
        (json!(5), json!(5)),
        "{r}"
    );
    assert_eq!(p.status()["files"], 5);
    let r = p.sync();
    assert_eq!(
        (r["need"].clone(), r["uploaded"].clone()),
        (json!(0), json!(0)),
        "{r}"
    );
    p.file("src/c.rs", b"pub fn helper() -> u32 {\n    2\n}\n");
    let r = p.sync();
    assert_eq!(
        (r["need"].clone(), r["uploaded"].clone()),
        (json!(1), json!(1)),
        "{r}"
    );
    std::fs::remove_file(p.repo.join("src/e.py")).unwrap();
    let r = p.sync();
    assert_eq!(
        (r["removed"].clone(), r["uploaded"].clone()),
        (json!(1), json!(0)),
        "{r}"
    );
    assert_eq!(p.status()["files"], 4);
    assert!(p.symbol_paths("eee").is_empty(), "удалённый файл в карте");
    let q = Proj::plain("incr-copy", &url, &tok);
    copy_dir(&p.repo, &q.repo);
    std::fs::remove_file(q.cache().join("code-state.json")).unwrap();
    let r = q.sync();
    assert_eq!(r["files"], 4, "{r}");
    assert_eq!(
        (r["need"].clone(), r["uploaded"].clone()),
        (json!(0), json!(0)),
        "{r}"
    );
}

// ---------- п. 7 ----------

/// §17 п. 7: минифицированный JS 10 МБ, вложенность скобок 100 000, бинарный `.rs`,
/// невалидный UTF-8, символическая ссылка за пределы корня, файл, вызывающий отсечку времени, —
/// синхронизация завершается, каждый файл пропущен с правильной причиной (невалидный UTF-8
/// разбирается, ссылка не обходится); обрыв процесса на файле после записи журнала падений →
/// следующий запуск пропускает его как `parser_crash`.
#[test]
fn acceptance_hostile_and_crash() {
    let _g = serial();
    let (url, tok) = start("hostile");
    let p = Proj::plain("hostile", &url, &tok);
    p.config("[code]\nmax_file_bytes = 16777216\nparse_timeout_ms = 10\n");
    let mut js = Vec::with_capacity(10 * 1024 * 1024 + 64);
    while js.len() < 10 * 1024 * 1024 {
        js.extend_from_slice(b"var a=function(b){return b+1};");
    }
    p.file("min.js", &js);
    p.file("nest.rs", "(\n".repeat(100_000).as_bytes());
    p.file("bin.rs", b"fn a() {}\0\0\x01\x02 binary\n");
    p.file(
        "bad.rs",
        b"pub fn valid_m56() {}\n// \xff\xfe invalid utf-8\n",
    );
    let mut slow = String::with_capacity(5 * 1024 * 1024);
    let mut i = 0u32;
    while slow.len() < 4 * 1024 * 1024 {
        slow.push_str(&format!(
            "pub fn function_{i}(x: u32) -> u32 {{ let y = x + {i}; y * 2 }}\n"
        ));
        i += 1;
    }
    p.file("slow.rs", slow.as_bytes());
    let outside = p.repo.parent().unwrap().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("far.rs"), b"pub fn far_m56() {}\n").unwrap();
    let linked = dir_link(&outside, &p.repo.join("link"));
    if !linked {
        eprintln!("символическую ссылку создать не удалось — проверка ссылки пропущена");
    }
    let r = p.sync();
    assert_eq!(r["files"], 5, "ссылка за пределы корня не обходится: {r}");
    let st = p.status();
    assert_eq!(
        reason_of(&st, "min.js").as_deref(),
        Some("minified"),
        "{st}"
    );
    assert_eq!(reason_of(&st, "bin.rs").as_deref(), Some("binary"), "{st}");
    assert_eq!(
        reason_of(&st, "slow.rs").as_deref(),
        Some("timeout"),
        "{st}"
    );
    assert_eq!(reason_of(&st, "bad.rs"), None, "{st}");
    assert_eq!(p.symbol_paths("valid_m56"), ["bad.rs"]);
    assert!(p.symbol_paths("far_m56").is_empty());
    // вложенность 100 000: разбор завершился (без падения процесса), файл в карте
    let files = p.get(&format!("/code/files?solution={G}&q=nest.rs"));
    assert!(files.to_string().contains("nest.rs"), "{files}");
    assert!(
        matches!(reason_of(&st, "nest.rs").as_deref(), None | Some("timeout")),
        "nest.rs: разобран (ok) или отсечка времени: {st}"
    );

    // обрыв процесса после записи журнала падений — только debug-сборка (крючок
    // `ZAM_DEBUG_CODE_ABORT`, решение составителя плана 17)
    if cfg!(debug_assertions) {
        let q = Proj::plain("crash", &url, &tok);
        three_files(&q);
        let o = q.zam_with(
            &["code", "sync", "--json"],
            b"",
            &[("ZAM_DEBUG_CODE_ABORT", "src/b.rs")],
        );
        assert_ne!(o.status.code(), Some(0), "процесс оборван: {}", err(&o));
        let j = json_of(&std::fs::read_to_string(q.cache().join("code-parsing.json")).unwrap());
        assert_eq!(j["path"], "src/b.rs", "{j}");
        let r = q.sync();
        assert_eq!(r["skipped"]["parser_crash"], 1, "{r}");
        assert_eq!(
            reason_of(&q.status(), "src/b.rs").as_deref(),
            Some("parser_crash")
        );
        assert_eq!(
            q.symbol_paths("helper"),
            ["src/c.rs"],
            "прочие файлы разобраны"
        );
    }
}

// ---------- п. 4 ----------

const A_RS: &str = "pub fn foo() -> u32 {\n    1\n}\n\npub fn bar() -> u32 {\n    2\n}\n";

/// §17 п. 4 через хуки: заметка `--ref src/a.rs#foo` и insight `--ref src/a.rs`. Правка `bar` —
/// `post-tool` кладёт в очередь хэши символов, `prompt` отправляет её: файловая ссылка `stale`,
/// символьная `ok`; правка `foo` — `symbol_changed`; удаление `foo` — `symbol_removed`; знание о
/// файле при чтении называет `foo`.
#[test]
fn acceptance_symbol_staleness_via_hook() {
    let _g = serial();
    let (url, tok, db) = start_db("hook");
    let p = Proj::plain("hook", &url, &tok);
    p.file("src/a.rs", A_RS.as_bytes());
    ok(&p.zam_with(
        &["write", "arch/foo.md", "--ref", "src/a.rs#foo"],
        "# О foo\nfoo возвращает единицу.\n".as_bytes(),
        &[],
    ));
    ok(&p.zam_with(
        &[
            "log",
            "insight",
            "--kind",
            "gotcha",
            "--title",
            "Файл a.rs целиком",
            "--ref",
            "src/a.rs",
            "--force",
        ],
        "Подробно о файле.\n".as_bytes(),
        &[],
    ));
    let st = |s: &str, r: Option<&str>| (s.to_string(), r.map(str::to_string));
    let states = |p: &Proj| {
        // индекс хуков и база сервера согласованы по состоянию
        let idx: Vec<(Option<String>, String)> = p
            .refs_of("src/a.rs")
            .into_iter()
            .map(|(sym, s, _)| (sym, s))
            .collect();
        let rows = db_refs(&db, "src/a.rs");
        let plain: Vec<(Option<String>, String)> = rows
            .iter()
            .map(|(sym, s, _)| (sym.clone(), s.clone()))
            .collect();
        assert_eq!(idx, plain, "индекс /file-refs и база");
        rows.into_iter()
            .map(|(sym, s, r)| (sym, (s, r)))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        states(&p),
        [(None, st("ok", None)), (Some("foo".into()), st("ok", None))]
    );
    // индекс знания о файлах — в кэш (старт сессии; хук fail-open — до 5 попыток, пока в кэше
    // не появится символьная ссылка: без неё `post-tool` не кладёт хэши символов)
    let cached = |p: &Proj| {
        std::fs::read_to_string(p.cache().join("file-refs.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .is_some_and(|v| {
                v["refs"].as_array().into_iter().flatten().any(|r| {
                    r["items"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|i| i["symbol"] == "foo")
                })
            })
    };
    for _ in 0..5 {
        ok(&p.hook(
            "session-start",
            &p.ev("SessionStart", "m56h", json!({"source": "startup"})),
        ));
        if cached(&p) {
            break;
        }
    }
    assert!(cached(&p), "индекс с символом не в кэше: {}", p.log());
    // очередь правок сессии `m56h` (`changed-m56h-<8 hex>.jsonl`)
    let queue_file = |p: &Proj| {
        std::fs::read_dir(p.cache())
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .find(|f| {
                f.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("changed-m56h-") && n.ends_with(".jsonl"))
            })
    };
    let edit = |body: &str| {
        p.file("src/a.rs", body.as_bytes());
        // `post-tool` разбирает файл в остатке своего бюджета (50 мс на процесс): отладочная
        // сборка под нагрузкой его исчерпывает — строка уходит без `symbols` и судится по хэшу
        // файла (консервативно, как M4.5). Сценарий п. 4 проверяет правило по символам: такая
        // строка снимается и правка повторяется (до 5 раз).
        let mut with_symbols = false;
        for _ in 0..5 {
            ok(&p.hook(
                "post-tool",
                &p.post_ok(
                    "m56h",
                    "Edit",
                    json!({"file_path": p.repo.join("src/a.rs"), "old_string": "x",
                           "new_string": "y"}),
                ),
            ));
            let f = queue_file(&p).unwrap_or_else(|| panic!("нет очереди: {}", p.log()));
            let text = std::fs::read_to_string(&f).unwrap();
            if text
                .lines()
                .last()
                .is_some_and(|l| l.contains("\"symbols\""))
            {
                with_symbols = true;
                break;
            }
            eprintln!("post-tool без symbols (бюджет исчерпан) — повтор правки");
            std::fs::remove_file(&f).unwrap();
        }
        assert!(with_symbols, "post-tool 5 раз без symbols; {}", p.log());
        ok(&p.hook(
            "prompt",
            &p.ev("UserPromptSubmit", "m56h", json!({"prompt": "дальше"})),
        ));
    };
    // правка `bar`
    edit(&A_RS.replace("2\n", "3\n"));
    assert_eq!(
        states(&p),
        [
            (None, st("stale", Some("source_changed"))),
            (Some("foo".into()), st("ok", None))
        ],
        "{}",
        p.log()
    );
    // правка `foo`
    edit(&A_RS.replace("2\n", "3\n").replace("1\n", "7\n"));
    assert_eq!(
        states(&p)[1],
        (Some("foo".into()), st("stale", Some("symbol_changed"))),
        "{}",
        p.log()
    );
    // удаление `foo`
    edit("pub fn bar() -> u32 {\n    3\n}\n");
    assert_eq!(
        states(&p)[1],
        (Some("foo".into()), st("stale", Some("symbol_removed"))),
        "{}",
        p.log()
    );
    // знание о файле при чтении называет `foo` (кэш обновляется стартом сессии)
    ok(&p.hook(
        "session-start",
        &p.ev("SessionStart", "m56r", json!({"source": "startup"})),
    ));
    let c = context(&p.hook(
        "post-tool",
        &p.post_ok(
            "m56r",
            "Read",
            json!({"file_path": p.repo.join("src/a.rs")}),
        ),
    ));
    assert!(c.contains("src/a.rs") && c.contains("`foo`"), "{c}");
}

// ---------- п. 8 ----------

/// Бюджет хука §10.3, мс (`Kind::budget_ms`); `post-tool` здесь — успешное чтение: без сети,
/// 50 мс на весь хук (M4.5 §9.1).
fn budget_ms(kind: &str) -> u64 {
    match kind {
        "session-start" | "subagent-start" => 300,
        "pre-tool" | "post-tool" => 50,
        _ => 250,
    }
}

/// §17 п. 8: бюджеты хуков с включённой и загруженной картой против живого сервера — конфигурация
/// клиента по умолчанию (`code_map` и `code.auto` включены), карта проекта синхронизирована.
///
/// Две проверки — раздельными вызовами (задача 17d, пункт g; принцип `hooks.rs`):
/// - **содержимое** — со щедрым бюджетом: `session-start` несёт строку карты, `subagent-start` с
///   задачей про существующую функцию — раздел `### Код`, в 12 из 12, **включая первые вызовы
///   после синхронизации** (с бюджетом продукта отладочная сборка на занятой машине молчит по
///   сроку — «карта в 9 из 10» было промахом бюджета, а не содержимого; настоящий дефект первого
///   запуска щедрый бюджет не скроет);
/// - **время** — с бюджетом продукта, внутри процесса (канал `ZAM_TEST_HOOK_TIMING`): медиана
///   серии не больше бюджета §10.3, p95 — запаса отладочной сборки ×4 (`session-start`/
///   `subagent-start` 1200 мс, `prompt` 1000 мс, `pre-tool` и успешный `post-tool` 200 мс) и
///   разброса пустого вызова в той же серии. Ловит: карта, вынесшая хук за бюджет (медиана), и редкий долгий путь
///   с картой (p95). Запуск процесса и release — `hooks.rs` и `cargo xtask code-bench`.
#[test]
fn acceptance_hook_budgets_with_map() {
    let _g = serial();
    let (url, tok) = start("budgets");
    let p = Proj::plain("budgets", &url, &tok);
    // конфигурация клиента по умолчанию: фоновая синхронизация включена
    std::fs::remove_file(p.home.join(".zeroaimemory/config.toml")).unwrap();
    three_files(&p);
    p.file(
        "src/d.ts",
        b"export function dee(): number {\n  return 4;\n}\n",
    );
    let r = p.sync();
    assert_eq!(r["uploaded"], 4, "{r}");
    let task = "Почини helper в src/c.rs: возвращает не то число";
    let event = |kind: &str, sess: &str, i: usize| -> Value {
        match kind {
            "session-start" => p.ev("SessionStart", sess, json!({"source": "startup"})),
            "prompt" => p.ev(
                "UserPromptSubmit",
                sess,
                json!({"prompt": "почему helper возвращает единицу"}),
            ),
            "pre-tool" => p.ev(
                "PreToolUse",
                sess,
                json!({"tool_name": "Read",
                       "tool_input": {"file_path": p.repo.join("src/c.rs")}}),
            ),
            "post-tool" => p.post_ok(sess, "Read", json!({"file_path": p.repo.join("src/c.rs")})),
            _ => {
                let spawn = p.ev(
                    "PreToolUse",
                    sess,
                    json!({"tool_name": "Agent", "tool_use_id": "u1",
                           "tool_input": {"description": "d", "prompt": task,
                                          "subagent_type": "Plan"}}),
                );
                ok(&p.hook("pre-tool", &spawn));
                p.ev(
                    "SubagentStart",
                    sess,
                    json!({"agent_id": format!("ag-{i}"), "agent_type": "Plan"}),
                )
            }
        }
    };
    let mut report = Vec::new();
    for (kind, want) in [
        ("session-start", Some("ZeroAIMemory: карта кода:")),
        ("prompt", None),
        ("pre-tool", None),
        ("post-tool", None),
        ("subagent-start", Some("### Код")),
    ] {
        // содержимое: щедрый бюджет, с первого вызова
        if let Some(w) = want {
            let hits: Vec<bool> = (0..12)
                .map(|i| {
                    let ev = event(kind, &format!("hb-{kind}-c{i}"), i);
                    context(&p.hook(kind, &ev)).contains(w)
                })
                .collect();
            assert!(
                hits.iter().all(|h| *h),
                "{kind}: «{w}» по вызовам {hits:?}; {}",
                p.log()
            );
        }
        // время: бюджет продукта, внутри процесса
        let (mut times, mut idle) = (Vec::new(), Vec::new());
        for i in 0..12 {
            let ev = event(kind, &format!("hb-{kind}-t{i}"), i);
            let mut e = p.hook_cmd(&p.url, kind, &json!({}));
            e.write_stdin(Vec::new());
            let t0 = std::time::Instant::now();
            let _ = e.output().unwrap();
            let empty_call = t0.elapsed();
            let (o, t) = common::hook_timed(p.hook_cmd(&p.url, kind, &ev), kind);
            ok(&o);
            // первые два — прогрев (первые запросы отладочного сервера, фоновая синхронизация)
            if i >= 2 {
                times.push(t.hook());
                idle.push(empty_call);
            }
        }
        let budget = Duration::from_millis(budget_ms(kind));
        let (mid, q) = (common::quantile(&times, 50), common::quantile(&times, 95));
        let noise = common::quantile(&idle, 95).saturating_sub(*idle.iter().min().unwrap());
        report.push(format!(
            "{kind} {}; пустой вызов {}",
            common::stats(&times),
            common::stats(&idle)
        ));
        assert!(
            mid <= budget,
            "{kind}: медиана {mid:?} при бюджете {budget:?}; {}",
            p.log()
        );
        assert!(
            q <= budget * 4 + noise,
            "{kind}: p95 {q:?} при пределе {:?} и разбросе пустого вызова {noise:?}; {}",
            budget * 4,
            p.log()
        );
    }
    eprintln!("ZAM_TIMING хуки с картой: {}", report.join("; "));
}

// ---------- п. 9 ----------

/// Ответ заглушки `/code/sync`: нужны оба файла.
const STUB_SYNC: &str = r#"{"need":["src/a.rs","src/long.ts"],"removed":0,"etag":"e","accepted":2,"stale_refs":0,"warnings":[]}"#;

fn keys(v: &Value) -> BTreeSet<String> {
    v.as_object()
        .unwrap_or_else(|| panic!("{v}"))
        .keys()
        .cloned()
        .collect()
}

/// Файлы п. 9: тело с маркером, длинная сигнатура TS и секрет в значении по умолчанию.
fn bodies_files(p: &Proj) {
    p.file(
        "src/a.rs",
        b"pub fn holder() -> &'static str {\n    \"BODY_MARKER_m56\"\n}\n",
    );
    let args: Vec<String> = (0..60).map(|i| format!("a{i}: number")).collect();
    p.file(
        "src/long.ts",
        format!(
            "export function longSignature({}) {{\n  return 'BODY_MARKER_m56';\n}}\n\
             export function withKey(key = \"{SECRET}\") {{\n  return key;\n}}\n",
            args.join(", ")
        )
        .as_bytes(),
    );
}

/// §17 п. 9: запросы `zam code sync` (заглушка) — только поля §4, без содержимого файлов, `sig`
/// ≤ 160 символов; `send_signatures = false` — без `sig`; на настоящем сервере секрет сигнатуры
/// в ответе `/code/symbols` замаскирован.
#[test]
fn acceptance_bodies_never_sent() {
    let _g = serial();
    for send_sig in [true, false] {
        let (url, seen) = common::stub(200, STUB_SYNC, Duration::ZERO);
        let Some(p) = Proj::git(&format!("bodies-{send_sig}"), &url, "zam_stub") else {
            return;
        };
        bodies_files(&p);
        p.commit();
        if !send_sig {
            p.config("[code]\nsend_signatures = false\n");
        }
        ok(&p.zam(&["code", "sync"]));
        let reqs = seen.lock().unwrap().clone();
        let sync: Vec<&Value> = reqs
            .iter()
            .filter(|(m, path, _)| m == "POST" && path == "/api/v1/code/sync")
            .map(|(_, _, b)| b)
            .collect();
        assert_eq!(sync.len(), 1);
        let allowed: BTreeSet<String> = ["branch", "commit", "machine", "manifest", "solution"]
            .map(String::from)
            .into();
        assert!(keys(sync[0]).is_subset(&allowed), "{}", sync[0]);
        for m in sync[0]["manifest"].as_array().unwrap() {
            assert_eq!(keys(m), ["hash", "path"].map(String::from).into());
        }
        let puts: Vec<&Value> = reqs
            .iter()
            .filter(|(m, path, _)| m == "PUT" && path == "/api/v1/code/files")
            .map(|(_, _, b)| b)
            .collect();
        assert_eq!(puts.len(), 1);
        let text = puts[0].to_string();
        assert!(!text.contains("BODY_MARKER_m56"), "тело ушло: {text}");
        assert!(!text.contains(SECRET), "секрет ушёл: {text}");
        let file_keys: BTreeSet<String> = [
            "git_oid",
            "hash",
            "lang",
            "parse_state",
            "path",
            "refs",
            "size",
            "skip_reason",
            "symbols",
        ]
        .map(String::from)
        .into();
        let sym_keys: BTreeSet<String> = [
            "body_hash",
            "end_line",
            "exported",
            "kind",
            "line",
            "name",
            "qual",
            "sig",
        ]
        .map(String::from)
        .into();
        let mut sigs = 0;
        for f in puts[0]["files"].as_array().unwrap() {
            assert!(keys(f).is_subset(&file_keys), "{f}");
            for s in f["symbols"].as_array().unwrap() {
                assert!(keys(s).is_subset(&sym_keys), "{s}");
                if let Some(sig) = s.get("sig") {
                    sigs += 1;
                    assert!(sig.as_str().unwrap().chars().count() <= 160, "{s}");
                }
            }
            for r in f["refs"].as_array().unwrap() {
                assert_eq!(
                    keys(r),
                    ["count", "first_line", "kind", "name"]
                        .map(String::from)
                        .into()
                );
            }
        }
        if send_sig {
            assert!(sigs >= 3, "{text}");
        } else {
            assert_eq!(sigs, 0, "{text}");
        }
    }
    // настоящий сервер: секрет сигнатуры в ответе `/code/symbols` замаскирован
    let (url, tok) = start("bodies");
    let p = Proj::plain("bodies-real", &url, &tok);
    bodies_files(&p);
    let r = p.sync();
    assert_eq!(r["uploaded"], 2, "{r}");
    let (code, v) = rest(
        &url,
        &tok,
        "GET",
        &format!("/code/symbols?solution={G}&q=withKey"),
        None,
    );
    assert_eq!(code, 200, "{v}");
    let text = v.to_string();
    assert!(text.contains("withKey"), "{text}");
    assert!(!text.contains(SECRET), "секрет в /code/symbols: {text}");
    assert!(!text.contains(&SECRET[..12]), "часть секрета: {text}");
}

// ---------- п. 12 ----------

/// §17 п. 12 (клиент): кэш `file-refs.json` клиента M4.5 (без поля `symbol`) читается новым
/// клиентом — подсказка о файле при чтении; строки очереди `changed-*.jsonl` без `symbols`
/// (клиент M4.5) уходят на сервер и помечают ссылку `stale`.
#[test]
fn acceptance_old_cache_reads() {
    let _g = serial();
    let p = Proj::plain("old-cache", DEAD, "zam_x");
    let c = json!({
        "etag": "f1", "fetched_at": "2026-10-29T00:00:00Z", "server": DEAD, "solution": G,
        "refs": [{"path": "src/pool.rs", "hash": "a".repeat(64), "items": [
            {"id": "i:E7", "kind": "gotcha", "title": "Пул соединений течёт", "state": "ok"},
            {"id": "n:N1", "kind": "note", "title": "Архитектура пула", "state": "stale"}]}],
    });
    std::fs::create_dir_all(p.cache()).unwrap();
    std::fs::write(p.cache().join("file-refs.json"), c.to_string()).unwrap();
    let ctx = context(&p.hook_at(
        DEAD,
        "post-tool",
        &p.post_ok(
            "oc1",
            "Read",
            json!({"file_path": p.repo.join("src/pool.rs")}),
        ),
    ));
    assert_eq!(
        ctx,
        "ZeroAIMemory: о файле src/pool.rs в памяти: Пул соединений течёт [gotcha] — i:E7; \
         Архитектура пула [note, возможно устарело] — n:N1"
    );
    // очередь клиента M4.5 — на настоящий сервер
    let (url, tok, db) = start_db("old-queue");
    let q = Proj::plain("old-queue", &url, &tok);
    q.file("src/x.rs", b"pub fn x() {}\n");
    ok(&q.zam_with(
        &[
            "log", "insight", "--kind", "gotcha", "--title", "О x", "--ref", "src/x.rs", "--force",
        ],
        "Подробно.\n".as_bytes(),
        &[],
    ));
    // файл очереди создаёт хук правки (имя — с хэшем сессии), строка подменяется старой
    let _ = q.hook_at(
        DEAD,
        "post-tool",
        &q.post_ok(
            "oq1",
            "Write",
            json!({"file_path": q.repo.join("src/x.rs")}),
        ),
    );
    let f = std::fs::read_dir(q.cache())
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("changed-oq1-") && n.ends_with(".jsonl"))
        })
        .unwrap_or_else(|| panic!("нет очереди: {}", q.log()));
    std::fs::write(
        &f,
        format!("{}\n", json!({"path": "src/x.rs", "hash": "1".repeat(64)})),
    )
    .unwrap();
    ok(&q.hook(
        "session-start",
        &q.ev("SessionStart", "oq2", json!({"source": "startup"})),
    ));
    assert!(!f.exists(), "очередь не ушла: {}", q.log());
    assert_eq!(q.refs_of("src/x.rs"), [(None, "stale".to_string(), None)]);
    assert_eq!(
        db_refs(&db, "src/x.rs"),
        [(
            None,
            "stale".to_string(),
            Some("source_changed".to_string())
        )]
    );
}
