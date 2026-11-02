//! `zam search` показывает карточки вложений (M5, задача 6): `a:<chunk>` с путём заметки, файлом
//! и подписью `locator`; `--json` — `kind: attachment`. Сервер — в том же процессе, рабочий процесс
//! разбора документов — бинарник `zam-server` из каталога сборки (без него тест пропускается:
//! `cargo test --workspace` собирает его для тестов `zam-server`).
mod common;
use std::process::Output;
use std::time::{Duration, Instant};

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn search_shows_attachment_cards() {
    let Some(worker) = common::docs_worker("search_shows_attachment_cards") else {
        return;
    };
    let data = common::sandbox("cli-search-att", "data");
    let cwd = common::sandbox("cli-search-att", "cwd");
    let (url, token) = common::docs_server(data, worker);
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
