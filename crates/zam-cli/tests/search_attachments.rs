//! `zam search` показывает карточки вложений (M5, задача 6): `a:<chunk>` с путём заметки, файлом
//! и подписью `locator`; `--json` — `kind: attachment`. Сервер — в том же процессе, рабочий процесс
//! разбора документов — бинарник `zam-server` из каталога сборки (без него тест пропускается:
//! `cargo test --workspace` собирает его для тестов `zam-server`).
mod common;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Output;
use std::time::{Duration, Instant};

use zam_server::build;
use zam_server::config::ServerConfig;

/// Сервер на 127.0.0.1:0 в фоновом потоке; адрес и первый токен администратора.
fn start(data: PathBuf, worker: PathBuf) -> (String, String) {
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
            cfg.docs.worker_program = Some(worker);
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

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn search_shows_attachment_cards() {
    let worker = assert_cmd::cargo::cargo_bin("zam-server");
    if !worker.is_file() {
        // как `common::ml::have` у zam-server: в CI и с ZAM_REQUIRE_ML пропуск недопустим
        assert!(
            std::env::var_os("ZAM_REQUIRE_ML").is_none() && std::env::var_os("CI").is_none(),
            "нет {}: соберите cargo build -p zam-server (или cargo test --workspace)",
            worker.display()
        );
        eprintln!(
            "SKIP search_shows_attachment_cards: нет {}",
            worker.display()
        );
        return;
    }
    let data = common::sandbox("cli-search-att", "data");
    let cwd = common::sandbox("cli-search-att", "cwd");
    let (url, token) = start(data, worker);
    let zam = |args: &[&str], stdin: &[u8]| -> Output {
        let o = common::zam_cmd()
            .env("ZAM_URL", &url)
            .env("ZAM_TOKEN", &token)
            .env("HOME", &cwd)
            .env("USERPROFILE", &cwd)
            .current_dir(&cwd)
            .args(args)
            .write_stdin(stdin.to_vec())
            .output()
            .unwrap();
        assert!(!out(&o).contains(&token) && !err(&o).contains(&token));
        o
    };
    let note = "knowledge:Документы/Склад";
    let o = zam(&["write", note], "# Склад\n\nОпись склада.\n".as_bytes());
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let csv = cwd.join("остатки.csv");
    std::fs::write(&csv, "Товар;Остаток\nгидрант;12\nвентиль;7\n").unwrap();
    let o = zam(&["attach", note, csv.to_str().unwrap()], b"");
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    // извлечение — фоном: ждём, пока поиск не найдёт фрагмент файла
    let t = Instant::now();
    let md = loop {
        let o = zam(&["search", "гидрант"], b"");
        let s = out(&o);
        if s.contains("a:") {
            break s;
        }
        assert!(
            t.elapsed() < Duration::from_secs(90),
            "вложение не нашлось: {s}\n{}",
            err(&o)
        );
        std::thread::sleep(Duration::from_millis(300));
    };
    let line = md
        .lines()
        .find(|l| l.contains(" a:"))
        .unwrap_or_else(|| panic!("{md}"));
    assert!(line.contains("/Документы/Склад › остатки.csv"), "{line}");
    assert!(line.contains("· лист «"), "{line}");
    let o = zam(&["search", "гидрант", "--json"], b"");
    let v: serde_json::Value = serde_json::from_str(&out(&o)).unwrap();
    let it = &v["items"][0];
    assert_eq!(it["kind"], "attachment", "{v}");
    assert_eq!(it["file"], "остатки.csv");
    assert!(it["id"].as_str().unwrap().starts_with("a:"), "{it}");
    assert!(
        it["locator"].as_str().unwrap().starts_with("sheet:"),
        "{it}"
    );
}
