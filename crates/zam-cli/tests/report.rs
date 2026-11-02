//! `zam report` (задача 5 M5.5, спецификация §6) против настоящего zam-server в том же процессе
//! тестов: картинки и файлы по относительным ссылкам — одним multipart, заголовок из `#`,
//! `--tag`/`--area`/`--ref`, пути с пробелами, `<…>`, `%20` и кириллицей, отказ враждебных путей
//! и пределов до любого запроса (заглушка без запросов), повтор — новый отчёт, `--json`.
//! PNG-фикстуры генерируются в тесте.
mod common;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::Value;
use zam_core::Id;
use zam_server::build;
use zam_server::config::ServerConfig;
use zam_store::{Access, EntryAttachment, entry_attachments, get_entry};

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e14";

/// Сервер в фоновом потоке: адрес, токен `rw` пользователя, его id, состояние и рантайм.
struct Srv {
    url: String,
    tok: String,
    user: Id,
    st: zam_server::AppState,
    rt: tokio::runtime::Handle,
}

impl Srv {
    fn start(name: &str) -> Srv {
        let data = common::sandbox("cli-report", &format!("{name}-data"));
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap();
            let handle = rt.handle().clone();
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
                tx.send((url, t, u.id, st.clone(), handle)).unwrap();
                axum::serve(
                    listener,
                    app.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .await
                .unwrap();
            });
        });
        let (url, tok, user, st, rt) = rx.recv().unwrap();
        Srv {
            url,
            tok,
            user,
            st,
            rt,
        }
    }

    fn db(&self) -> zam_store::Db {
        self.rt.block_on(self.st.users.get(self.user)).unwrap()
    }

    fn files(&self, id: &str) -> Vec<EntryAttachment> {
        let db = self.db();
        self.rt
            .block_on(entry_attachments(&db, id.parse().unwrap()))
            .unwrap()
    }

    fn entry(&self, id: &str) -> zam_store::Entry {
        let db = self.db();
        self.rt
            .block_on(get_entry(&db, id.parse().unwrap()))
            .unwrap()
    }

    fn reports(&self) -> i64 {
        let db = self.db();
        self.rt
            .block_on(db.read(|c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM entries WHERE kind = 'report'",
                    [],
                    |r| r.get::<_, i64>(0),
                )?)
            }))
            .unwrap()
    }
}

/// Каталог проекта с `solution.toml` решения `G`.
fn project(name: &str) -> PathBuf {
    let d = common::sandbox("cli-report", name);
    std::fs::create_dir_all(d.join(".zeroaimemory")).unwrap();
    std::fs::write(
        d.join(".zeroaimemory").join("solution.toml"),
        format!("agent_solution_id = \"{G}\"\nname = \"Отчёты\"\n"),
    )
    .unwrap();
    d
}

/// PNG-подобные байты: сигнатура и `n` байт по `seed`.
fn png(seed: u8, n: usize) -> Vec<u8> {
    let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
    v.extend((0..n).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)));
    v
}

fn put(dir: &Path, rel: &str, data: &[u8]) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, data).unwrap();
}

fn zam(url: &str, tok: &str, cwd: &Path, args: &[&str]) -> Output {
    let mut c = common::zam_cmd();
    c.env("ZAM_URL", url)
        .env("ZAM_TOKEN", tok)
        .env("HOME", cwd)
        .env("USERPROFILE", cwd)
        .current_dir(cwd)
        .args(args)
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

/// Код 6 (неверный ввод).
fn input_err(o: &Output) -> String {
    assert_eq!(
        o.status.code(),
        Some(6),
        "stdout: {}\nstderr: {}",
        out(o),
        err(o)
    );
    err(o)
}

/// Id отчёта из строки `Отчёт: <url>` (последний сегмент адреса).
fn id_of(stdout: &str) -> String {
    let url = stdout
        .trim()
        .strip_prefix("Отчёт: ")
        .unwrap_or_else(|| panic!("нет «Отчёт:»: {stdout}"));
    assert!(url.contains(&format!("/s/{G}/j/reports/")), "{url}");
    url.rsplit('/').next().unwrap().to_string()
}

fn names(files: &[EntryAttachment]) -> Vec<String> {
    let mut v: Vec<String> = files.iter().map(|f| f.filename.clone()).collect();
    v.sort();
    v
}

/// Заглушка без ответа по существу: запросов к ней быть не должно.
fn no_server() -> (String, common::Seen) {
    common::stub(500, "{}", std::time::Duration::ZERO)
}

#[test]
fn cli_report_uploads_images_prints_url() {
    let s = Srv::start("upload");
    let d = project("upload");
    for (i, n) in ["figs/loss.png", "figs/acc.png", "figs/lr.png"]
        .iter()
        .enumerate()
    {
        put(&d, n, &png(i as u8, 200 + i));
    }
    put(
        &d,
        "report.md",
        "# Обучение модели\n\nПотери $L = -\\sum_i y_i \\log p_i$ падают.\n\n\
         ```mermaid\ngraph TD; A-->B\n```\n\n![потери](figs/loss.png)\n\n\
         ![точность](figs/acc.png \"точность\")\n\n![шаг](./figs/lr.png)\n"
            .as_bytes(),
    );
    let o = zam(&s.url, &s.tok, &d, &["report", "report.md"]);
    let stdout = ok(&o);
    assert!(stdout.starts_with(&format!("Отчёт: {}", s.url)), "{stdout}");
    let id = id_of(&stdout);
    let files = s.files(&id);
    assert_eq!(names(&files), ["acc.png", "loss.png", "lr.png"]);
    assert!(files.iter().all(|f| f.mime == "image/png"), "{files:?}");
    let e = s.entry(&id);
    assert_eq!(e.title, "Обучение модели");
    assert!(e.body.contains("_attachments/"), "{}", e.body);
    assert!(e.body.contains("```mermaid"), "{}", e.body);
    assert!(e.body.contains("\\log p_i$"), "{}", e.body);
}

#[test]
fn cli_report_title_from_first_h1_or_flag() {
    let s = Srv::start("title");
    let d = project("title");
    put(
        &d,
        "r.md",
        "Вступление\n\n## Раздел\n\n# Итоги эксперимента\n".as_bytes(),
    );
    let id = id_of(&ok(&zam(&s.url, &s.tok, &d, &["report", "r.md"])));
    assert_eq!(s.entry(&id).title, "Итоги эксперимента");
    let id = id_of(&ok(&zam(
        &s.url,
        &s.tok,
        &d,
        &["report", "r.md", "--title", "Свой заголовок"],
    )));
    assert_eq!(s.entry(&id).title, "Свой заголовок");
}

#[test]
fn cli_report_no_title_error_input() {
    let (url, seen) = no_server();
    let d = project("no-title");
    put(
        &d,
        "r.md",
        "Текст без заголовка\n\n```\n# в коде\n```\n".as_bytes(),
    );
    let e = input_err(&zam(&url, "zam_x", &d, &["report", "r.md"]));
    assert!(e.contains("--title"), "{e}");
    assert!(seen.lock().unwrap().is_empty(), "запрос к серверу");
}

#[test]
fn cli_report_refs_tags_area() {
    let s = Srv::start("refs");
    let d = project("refs");
    put(&d, "src/main.rs", b"fn main() {}\n");
    put(&d, "r.md", "# Сравнение\n\nИтог.\n".as_bytes());
    let o = zam(
        &s.url,
        &s.tok,
        &d,
        &[
            "report",
            "r.md",
            "--tag",
            "ml",
            "--tag",
            "сравнение",
            "--area",
            "core",
            "--ref",
            "commit:abc123",
            "--ref",
            "file:src/main.rs",
            "--ref",
            "note:01J0000000000000000000000N",
            "--ref",
            "task:k:01J0000000000000000000000K",
        ],
    );
    let id = id_of(&ok(&o));
    let e = s.entry(&id);
    assert_eq!(e.tags, ["ml", "сравнение"]);
    assert_eq!(e.area.as_deref(), Some("core"));
    let refs = &e.data["refs"];
    assert_eq!(refs["commits"], serde_json::json!(["abc123"]));
    // путь — от корня проекта по правилам хуков (у песочницы корень — каталог с `.git`)
    let files = refs["files"].as_array().unwrap();
    assert_eq!(files.len(), 1, "{refs}");
    assert!(
        files[0].as_str().unwrap().ends_with("src/main.rs"),
        "{refs}"
    );
    assert_eq!(
        refs["notes"],
        serde_json::json!(["01J0000000000000000000000N"])
    );
    assert_eq!(
        refs["tasks"],
        serde_json::json!(["k:01J0000000000000000000000K"])
    );
    // неизвестный вид ссылки — ошибка ввода
    let (url, seen) = no_server();
    let e = input_err(&zam(
        &url,
        "zam_x",
        &d,
        &["report", "r.md", "--ref", "issue:7"],
    ));
    assert!(e.contains("--ref"), "{e}");
    assert!(seen.lock().unwrap().is_empty());
}

#[test]
fn cli_report_paths_with_spaces_angle_and_percent() {
    let s = Srv::start("paths");
    let d = project("paths");
    put(&d, "figs/my plot.png", &png(1, 64));
    put(&d, "figs/second plot.png", &png(2, 64));
    put(&d, "figs/рис 1.png", &png(3, 64));
    put(&d, "data/run 1.csv", b"step,loss\n1,0.5\n");
    put(
        &d,
        "r.md",
        "# Пути\n\n![a](<figs/my plot.png>)\n![b](figs/second%20plot.png)\n\
         ![в](<figs/рис 1.png>)\n[данные](data/run%201.csv)\n"
            .as_bytes(),
    );
    let id = id_of(&ok(&zam(&s.url, &s.tok, &d, &["report", "r.md"])));
    let files = s.files(&id);
    assert_eq!(
        names(&files),
        ["my plot.png", "run 1.csv", "second plot.png", "рис 1.png"]
    );
    // отчёт из подкаталога: ссылки — от каталога файла, а не от текущего
    put(&d, "sub/figs/x.png", &png(4, 32));
    put(
        &d,
        "sub/r.md",
        "# Подкаталог\n\n![x](figs/x.png)\n".as_bytes(),
    );
    let id = id_of(&ok(&zam(&s.url, &s.tok, &d, &["report", "sub/r.md"])));
    assert_eq!(names(&s.files(&id)), ["x.png"]);
}

#[test]
fn cli_report_rejects_dotdot_absolute_backslash_symlink_outside() {
    let (url, seen) = no_server();
    let root = project("hostile");
    put(&root, "outside.png", &png(1, 32));
    let d = root.join("rep");
    put(&d, "figs/ok.png", &png(2, 32));
    let abs = root.join("outside.png");
    let abs = abs.to_string_lossy().replace('\\', "/");
    let mut cases = vec![
        "![a](../outside.png)".to_string(),
        "![a](figs/../../outside.png)".to_string(),
        format!("![a](<{abs}>)"),
        "![a](/etc/passwd)".to_string(),
        "![a](C:/Windows/win.ini)".to_string(),
        "![a](figs\\ok.png)".to_string(),
        "![a](figs%5Cok.png)".to_string(),
        "[a](figs/a:b.png)".to_string(),
    ];
    let link = d.join("figs").join("link.png");
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(root.join("outside.png"), &link).is_ok();
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_file(root.join("outside.png"), &link).is_ok();
    if made {
        cases.push("![a](figs/link.png)".to_string());
    } else {
        eprintln!("символическая ссылка не создана — случай пропущен");
    }
    for c in &cases {
        put(
            &d,
            "r.md",
            format!("# Т\n\n![ok](figs/ok.png)\n{c}\n").as_bytes(),
        );
        let e = input_err(&zam(&url, "zam_x", &root, &["report", "rep/r.md"]));
        assert!(!e.is_empty(), "{c}");
    }
    assert!(seen.lock().unwrap().is_empty(), "запрос к серверу");
}

#[test]
fn cli_report_duplicate_image_one_part() {
    let s = Srv::start("dup");
    let d = project("dup");
    put(&d, "figs/a.png", &png(1, 50));
    put(
        &d,
        "r.md",
        "# Дубль\n\n![1](figs/a.png)\n![2](./figs/a.png)\n![3](figs/%61.png)\n\
         [ref][x]\n\n[x]: <figs/a.png>\n"
            .as_bytes(),
    );
    // сервер отвечает 400 на повтор части — успех значит одна часть
    let id = id_of(&ok(&zam(&s.url, &s.tok, &d, &["report", "r.md"])));
    assert_eq!(names(&s.files(&id)), ["a.png"]);
}

#[test]
fn cli_report_skips_urls_anchors_code() {
    let s = Srv::start("skip");
    let d = project("skip");
    put(&d, "figs/real.png", &png(1, 40));
    put(
        &d,
        "r.md",
        "# Ссылки\n\n[сайт](https://example.com/a.png) [почта](mailto:a@b.c) [раздел](#ссылки)\n\
         `![код](missing/inline.png)`\n\n```\n![блок](missing/fence.png)\n```\n\n\
         \x20   ![отступ](missing/indent.png)\n\n![real](figs/real.png)\n"
            .as_bytes(),
    );
    let id = id_of(&ok(&zam(&s.url, &s.tok, &d, &["report", "r.md"])));
    assert_eq!(names(&s.files(&id)), ["real.png"]);
}

#[test]
fn cli_report_missing_file_error() {
    let (url, seen) = no_server();
    let d = project("missing");
    put(&d, "r.md", "# Т\n\n![нет](figs/none.png)\n".as_bytes());
    let e = input_err(&zam(&url, "zam_x", &d, &["report", "r.md"]));
    assert!(e.contains("не найден"), "{e}");
    // нет и самого файла отчёта
    let e = input_err(&zam(&url, "zam_x", &d, &["report", "nope.md"]));
    assert!(e.contains("nope.md"), "{e}");
    assert!(seen.lock().unwrap().is_empty(), "запрос к серверу");
}

#[test]
fn cli_report_limits_checked_locally() {
    let (url, seen) = no_server();
    let d = project("limits");
    let mut md = String::from("# Много\n\n");
    for i in 0..65 {
        put(&d, &format!("figs/{i}.png"), &png(i as u8, 8));
        md.push_str(&format!("![{i}](figs/{i}.png)\n"));
    }
    put(&d, "r.md", md.as_bytes());
    let e = input_err(&zam(&url, "zam_x", &d, &["report", "r.md"]));
    assert!(e.contains("64"), "{e}");
    assert!(seen.lock().unwrap().is_empty(), "запрос к серверу");
}

#[test]
fn cli_report_twice_two_reports() {
    let s = Srv::start("twice");
    let d = project("twice");
    put(&d, "figs/a.png", &png(1, 30));
    put(&d, "r.md", "# Повтор\n\n![a](figs/a.png)\n".as_bytes());
    let a = id_of(&ok(&zam(&s.url, &s.tok, &d, &["report", "r.md"])));
    let b = id_of(&ok(&zam(&s.url, &s.tok, &d, &["report", "r.md"])));
    assert_ne!(a, b, "без --key повтор — новый отчёт");
    assert_eq!(s.reports(), 2);
    // с ключом — тот же отчёт
    let k1 = id_of(&ok(&zam(
        &s.url,
        &s.tok,
        &d,
        &["report", "r.md", "--key", "rep-1"],
    )));
    let k2 = id_of(&ok(&zam(
        &s.url,
        &s.tok,
        &d,
        &["report", "r.md", "--key", "rep-1"],
    )));
    assert_eq!(k1, k2);
    assert_eq!(s.reports(), 3);
}

#[test]
fn cli_report_json_output() {
    let s = Srv::start("json");
    let d = project("json");
    put(&d, "r.md", "# JSON\n\nТекст.\n".as_bytes());
    let o = zam(&s.url, &s.tok, &d, &["report", "r.md", "--json"]);
    let v: Value = serde_json::from_str(&ok(&o)).unwrap();
    let obj = v.as_object().unwrap();
    assert_eq!(obj.len(), 2, "{v}");
    let id = v["id"].as_str().unwrap();
    assert!(
        v["url"]
            .as_str()
            .unwrap()
            .ends_with(&format!("/j/reports/{id}")),
        "{v}"
    );
    // тело отчёта агенту не возвращается
    assert!(!o.stdout.is_empty() && !out(&o).contains("Текст."), "{v}");
}
