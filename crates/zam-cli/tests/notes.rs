//! Команды заметок `zam` (tree, read, write, history, search, attach) против настоящего
//! zam-server в том же процессе тестов. Сервер крутится в отдельном потоке со своим
//! рантаймом tokio, поэтому сами тесты синхронные.
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::Command;
use zam_server::build;
use zam_server::config::ServerConfig;

fn tmp(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("cli-notes-{name}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Сервер на 127.0.0.1:0 в фоновом потоке; адрес и первый токен администратора.
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
            let (app, _st, rep) = build(cfg, Some("pw".into())).await.unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            tx.send((url, rep.initial_token.unwrap())).unwrap();
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

/// Подключение для команд: адрес и токен через окружение (как у агента).
struct Env {
    url: String,
    token: String,
    cwd: PathBuf,
}

impl Env {
    fn new(name: &str) -> Env {
        let (url, token) = start(name);
        Env {
            url,
            token,
            cwd: tmp(&format!("{name}-cwd")),
        }
    }

    /// `zam` с чистым окружением: без прокси и ZAM_*, HOME и cwd — во временном каталоге.
    fn zam(&self) -> Command {
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
        c.env("ZAM_URL", &self.url)
            .env("ZAM_TOKEN", &self.token)
            .env("HOME", &self.cwd)
            .env("USERPROFILE", &self.cwd)
            .current_dir(&self.cwd);
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        let o = self.zam().args(args).output().unwrap();
        self.no_token(&o);
        o
    }

    fn run_in(&self, cwd: &Path, args: &[&str], stdin: &[u8]) -> Output {
        let o = self
            .zam()
            .current_dir(cwd)
            .args(args)
            .write_stdin(stdin.to_vec())
            .output()
            .unwrap();
        self.no_token(&o);
        o
    }

    fn run_stdin(&self, args: &[&str], stdin: &[u8]) -> Output {
        self.run_in(&self.cwd.clone(), args, stdin)
    }

    fn no_token(&self, o: &Output) {
        assert!(
            !out(o).contains(&self.token) && !err(o).contains(&self.token),
            "токен в выводе"
        );
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

/// `<id>@<rev>` из вывода write/revert.
fn id_rev(s: &str) -> (String, i64) {
    let (id, rev) = s.trim().split_once('@').expect("ожидается <id>@<rev>");
    assert_eq!(id.len(), 26, "{s}");
    (id.to_string(), rev.parse().unwrap())
}

const NOTE: &str = "knowledge:Инструменты/Bash скрипты";
const TEXT1: &[u8] = "# Bash скрипты\r\n\r\nПолезные скрипты для bash.\r\n".as_bytes();
const TEXT2: &[u8] = "# Bash скрипты\r\n\r\nПолезные скрипты для bash и zsh.\r\n".as_bytes();

#[test]
fn notes_full_cycle() {
    let e = Env::new("cycle");

    // запись из stdin: новая заметка → rev 1
    let (id, rev) = id_rev(&ok(&e.run_stdin(&["write", NOTE, "-m", "первая"], TEXT1)));
    assert_eq!(rev, 1);

    // чтение побайтно, с CRLF, без лишнего перевода строки
    let o = e.run(&["read", NOTE]);
    ok(&o);
    assert_eq!(o.stdout, TEXT1);
    // по id — то же
    assert_eq!(e.run(&["read", &id]).stdout, TEXT1);

    // вторая запись (чужая правка) → rev 2, тот же id
    let (id2, rev) = id_rev(&ok(&e.run_stdin(&["write", NOTE], TEXT2)));
    assert_eq!((id2.as_str(), rev), (id.as_str(), 2));

    // запись от устаревшей ревизии → 409: код 4 и дифф в stderr
    let o = e.run_stdin(&["write", NOTE, "--base-rev", "1"], b"# other\n");
    assert_eq!(o.status.code(), Some(4), "{}", err(&o));
    let se = err(&o);
    assert!(se.contains("--- rev 1") && se.contains("+++ rev 2"), "{se}");
    assert!(se.contains("zsh"), "{se}");

    // история: 2 ревизии, новые первыми
    let s = ok(&e.run(&["history", NOTE]));
    let lines: Vec<&str> = s.lines().collect();
    assert_eq!(lines.len(), 2, "{s}");
    assert!(
        lines[0].starts_with("2\t") && lines[1].starts_with("1\t"),
        "{s}"
    );
    assert!(
        lines[1].contains("agent") && lines[1].contains("первая"),
        "{s}"
    );

    // дифф ревизий
    let s = ok(&e.run(&["history", NOTE, "--diff", "1..2"]));
    assert!(s.contains("+++ rev 2") && s.contains("zsh"), "{s}");

    // откат к 1 → rev 3 с текстом rev 1
    let (_, rev) = id_rev(&ok(&e.run(&["history", NOTE, "--revert", "1"])));
    assert_eq!(rev, 3);
    assert_eq!(e.run(&["read", NOTE]).stdout, TEXT1);
    assert_eq!(e.run(&["read", NOTE, "--rev", "2"]).stdout, TEXT2);

    // поиск находит путь (md по умолчанию)
    let s = ok(&e.run(&["search", "скрипты"]));
    assert!(s.contains("Инструменты/Bash скрипты"), "{s}");
    let v: serde_json::Value =
        serde_json::from_str(&ok(&e.run(&["search", "скрипты", "--json"]))).unwrap();
    assert_eq!(v["items"][0]["path"], "Инструменты/Bash скрипты");

    // дерево показывает папку, вглубь — заметку
    let s = ok(&e.run(&["tree", "knowledge:"]));
    assert!(s.contains("Инструменты/"), "{s}");
    let s = ok(&e.run(&["tree", "knowledge:", "--depth", "2"]));
    assert!(s.contains("Bash скрипты"), "{s}");
    let s = ok(&e.run(&["tree", "knowledge:Инструменты"]));
    assert!(
        s.contains("Bash скрипты") && !s.contains("Инструменты/"),
        "{s}"
    );

    // вложение PNG: печатается ссылка для вставки
    let png = e.cwd.join("схема 1.png");
    std::fs::write(&png, b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR-test-bytes").unwrap();
    let s = ok(&e.run(&["attach", NOTE, png.to_str().unwrap()]));
    assert!(s.trim().starts_with("_attachments/"), "{s}");
    assert!(s.trim().ends_with(".png"), "{s}");

    // несколько целей: заголовок перед каждой
    let s = ok(&e.run(&["read", NOTE, &id]));
    assert!(s.starts_with(&format!("--- {NOTE} ---\n")), "{s}");
    assert!(s.contains(&format!("--- {id} ---\n")), "{s}");
}

#[test]
fn read_tiers_section_and_json() {
    let e = Env::new("tiers");
    let text = "# A\nПервое. Второе.\n## B\nТекст B.\n";
    ok(&e.run_stdin(&["write", "knowledge:t", "--l0", "сводка"], text.as_bytes()));
    assert_eq!(
        ok(&e.run(&["read", "knowledge:t", "--tier", "L0"])),
        "сводка\n"
    );
    let s = ok(&e.run(&["read", "knowledge:t", "--tier", "l1"]));
    // L1 — note_l1 (§8.3): «<отступ><заголовок> — <первое предложение>»
    assert!(s.starts_with("A — Первое.\n  B — Текст B."), "{s}");
    assert_eq!(
        ok(&e.run(&["read", "knowledge:t", "--section", "A > B"])),
        "## B\nТекст B.\n"
    );
    let v: serde_json::Value =
        serde_json::from_str(&ok(&e.run(&["read", "knowledge:t", "--json"]))).unwrap();
    assert_eq!(v["content"], text);
    assert_eq!(v["rev"], 1);
}

#[test]
fn write_from_file_and_json_output() {
    let e = Env::new("file");
    let f = e.cwd.join("n.md");
    std::fs::write(&f, b"\xEF\xBB\xBF# BOM\r\n").unwrap();
    let s = ok(&e.run(&[
        "write",
        "knowledge:f",
        "--file",
        f.to_str().unwrap(),
        "--json",
    ]));
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert_eq!(v["rev"], 1);
    assert_eq!(v["created"], true);
    assert_eq!(v["path"], "f");
    assert_eq!(
        e.run(&["read", "knowledge:f"]).stdout,
        b"\xEF\xBB\xBF# BOM\r\n"
    );
}

#[test]
fn errors_have_stable_exit_codes() {
    let e = Env::new("codes");
    // нет заметки → 5
    let o = e.run(&["read", "knowledge:нет/такой"]);
    assert_eq!(o.status.code(), Some(5), "{}", err(&o));
    let o = e.run(&["history", "knowledge:нет"]);
    assert_eq!(o.status.code(), Some(5), "{}", err(&o));
    // не UTF-8 во входе → 6
    let o = e.run_stdin(&["write", "knowledge:bin"], b"\xff\xfe\x00");
    assert_eq!(o.status.code(), Some(6), "{}", err(&o));
    // некорректный путь → 6
    let o = e.run_stdin(&["write", "knowledge:a//b"], b"x");
    assert_eq!(o.status.code(), Some(6), "{}", err(&o));
    // некорректный --diff → 6
    let o = e.run(&["history", "knowledge:x", "--diff", "abc"]);
    assert_eq!(o.status.code(), Some(6), "{}", err(&o));
    // solution: без solution.toml → 1
    let o = e.run(&["read", "solution:x"]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(err(&o).contains("solution.toml"), "{}", err(&o));
    // неверный токен → 3
    let o = e
        .zam()
        .env("ZAM_TOKEN", "zam_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghij0123")
        .args(["tree"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(3), "{}", err(&o));
    // нет токена → 3
    let o = e
        .zam()
        .env_remove("ZAM_TOKEN")
        .args(["tree"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(3), "{}", err(&o));
    // Human по токену не виден → 5
    let o = e.run(&["tree", "human:"]);
    assert_eq!(o.status.code(), Some(5), "{}", err(&o));
    // ошибка в аргументах → 6 (а не 2 — «сервер недоступен»)
    let o = e.run(&["read"]);
    assert_eq!(o.status.code(), Some(6), "{}", err(&o));
}

#[test]
fn unreachable_server_exit_2() {
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let cwd = tmp("down-cwd");
    let o = Command::cargo_bin("zam")
        .unwrap()
        .env_remove("ZAM_URL")
        .env_remove("ZAM_TOKEN")
        .env("HOME", &cwd)
        .env("USERPROFILE", &cwd)
        .current_dir(&cwd)
        .args([
            "search",
            "x",
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
fn solution_toml_scopes_write_and_search() {
    let e = Env::new("solution");
    let repo = e.cwd.join("repo");
    let sub = repo.join("src");
    std::fs::create_dir_all(repo.join(".zeroaimemory")).unwrap();
    std::fs::create_dir_all(&sub).unwrap();
    let guid = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e11";
    std::fs::write(
        repo.join(".zeroaimemory").join("solution.toml"),
        format!("id = \"{guid}\"\nname = \"TradingPlatform\"\n"),
    )
    .unwrap();

    // без префикса — в решение (регистрируется автоматически)
    let o = e.run_in(&sub, &["write", "x"], "# Квазар\nрешение\n".as_bytes());
    ok(&o);
    let o = e.run_in(&sub, &["read", "solution:x"], b"");
    assert_eq!(o.stdout, "# Квазар\nрешение\n".as_bytes());
    let o = e.run_in(&sub, &["read", &format!("{guid}:x")], b"");
    assert_eq!(o.stdout, "# Квазар\nрешение\n".as_bytes());
    // в knowledge её нет
    let o = e.run_in(&sub, &["read", "knowledge:x"], b"");
    assert_eq!(o.status.code(), Some(5));

    // поиск из каталога решения находит, вне его — нет (решение не в области)
    let s = ok(&e.run_in(&sub, &["search", "квазар"], b""));
    assert!(s.contains("TradingPlatform/x"), "{s}");
    let o = e.run(&["search", "квазар"]);
    let s = ok(&o);
    assert!(!s.contains("/x"), "{s}");
    // пустая выдача без моделей: «ничего не найдено» и строка деградации
    assert!(err(&o).contains("ничего не найдено"), "{}", err(&o));
    assert!(
        s.starts_with("> degraded: ") && s.trim_end().ends_with("(только FTS)"),
        "{s}"
    );
    // --all — все решения
    let s = ok(&e.run(&["search", "квазар", "--all"]));
    assert!(s.contains("TradingPlatform/x"), "{s}");
    // деградация — и человеческой строкой в stderr (stdout — ответ сервера как есть)
    let o = e.run(&["search", "квазар", "--all"]);
    assert!(
        err(&o).contains("векторный поиск недоступен: модели не загружены"),
        "{}",
        err(&o)
    );

    // --rerank: серверу уходит rerank=on; без модели реранкера — предупреждение
    // сервера в конце md и по-русски в stderr, выдача та же, код 0
    let o = e.run(&["search", "квазар", "--all", "--rerank"]);
    let s = ok(&o);
    assert!(s.contains("TradingPlatform/x"), "{s}");
    assert!(s.contains("> warning: reranker"), "{s}");
    assert!(err(&o).contains("предупреждение: реранкер"), "{}", err(&o));
    // в JSON — поле warnings и та же строка в stderr
    let o = e.run(&["search", "квазар", "--all", "--rerank", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&ok(&o)).unwrap();
    assert_eq!(v["degraded"], true);
    assert!(
        v["warnings"][0].as_str().unwrap().starts_with("reranker"),
        "{v}"
    );
    assert!(err(&o).contains("предупреждение: реранкер"), "{}", err(&o));
    assert!(
        err(&o).contains("векторный поиск недоступен"),
        "{}",
        err(&o)
    );
    // без --rerank предупреждения нет
    let o = e.run(&["search", "квазар", "--all", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&ok(&o)).unwrap();
    assert_eq!(v["warnings"], serde_json::json!([]));

    // атрибуция машины: X-Zam-Machine из COMPUTERNAME/HOSTNAME
    let o = e
        .zam()
        .current_dir(&sub)
        .env("COMPUTERNAME", "box-1")
        .env("HOSTNAME", "box-1")
        .args(["write", "x"])
        .write_stdin("# Квазар\nдва\n")
        .output()
        .unwrap();
    ok(&o);
    let s = ok(&e.run_in(&sub, &["history", "x"], b""));
    assert!(s.lines().next().unwrap().contains("box-1"), "{s}");

    // битый solution.toml → 1
    std::fs::write(repo.join(".zeroaimemory").join("solution.toml"), "id = [").unwrap();
    let o = e.run_in(&sub, &["tree"], b"");
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
}

/// Пачка (`POST /read`): заголовки элементов, `--max-tokens`, `--if-rev`, ошибка по элементу
/// не обрывает остальные (код 5), `--json` и несовместимость с `--rev`.
#[test]
fn read_batch_flags() {
    let e = Env::new("batch");
    let text = "# A\nПервое. Второе.\n## B\nТекст B.\n";
    ok(&e.run_stdin(&["write", "knowledge:b1"], text.as_bytes()));
    ok(&e.run_stdin(&["write", "knowledge:b2"], "# Два\n".as_bytes()));
    let s = ok(&e.run(&["read", "knowledge:b1", "knowledge:b2", "--batch"]));
    assert!(s.contains("Knowledge/b1  rev1 · ~"), "{s}");
    assert!(s.contains("Knowledge/b2  rev1 · ~"), "{s}");
    assert!(s.contains("Текст B."), "{s}");
    assert!(
        s.trim_end().lines().last().unwrap().ends_with(" tok"),
        "{s}"
    );
    // L1 и секция относятся ко всем целям
    let s = ok(&e.run(&["read", "knowledge:b1", "--tier", "l1", "--batch"]));
    assert!(s.contains("A — Первое."), "{s}");
    // если ревизия не изменилась — без текста
    let s = ok(&e.run(&["read", "knowledge:b1", "--if-rev", "1"]));
    assert!(
        s.contains("rev1 · unchanged") && !s.contains("Текст B."),
        "{s}"
    );
    // обрезка по токенам
    let s = ok(&e.run(&["read", "knowledge:b1", "--max-tokens", "3"]));
    assert!(s.contains("…ещё "), "{s}");
    // нет заметки: остальные читаются, код 5
    let o = e.run(&["read", "knowledge:нет", "knowledge:b2", "--batch"]);
    assert_eq!(o.status.code(), Some(5), "{}", err(&o));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        out.contains("error: not_found") && out.contains("# Два"),
        "{out}"
    );
    // json — ответ сервера
    let v: serde_json::Value =
        serde_json::from_str(&ok(&e.run(&["read", "knowledge:b2", "--batch", "--json"]))).unwrap();
    assert_eq!(v["items"][0]["text"], "# Два\n");
    // --rev с пачкой
    let o = e.run(&["read", "knowledge:b1", "--batch", "--rev", "1"]);
    assert_eq!(o.status.code(), Some(6), "{}", err(&o));
}
