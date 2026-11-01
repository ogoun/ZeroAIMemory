//! `zam read` вложений (M5, задача 7): `a:<chunk>` из выдачи — фрагмент по `locator` чанка,
//! `b:<sha256> --locator <loc>` — часть документа, без `--locator` — оглавление; `--locator` у
//! заметки — код 6 (`locator-invalid`). Сервер — в том же процессе, рабочий процесс разбора —
//! бинарник `zam-server` из каталога сборки (без него тест пропускается).
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
fn cli_read_locator() {
    let Some(worker) = common::docs_worker("cli_read_locator") else {
        return;
    };
    let data = common::sandbox("cli-read-loc", "data");
    let cwd = common::sandbox("cli-read-loc", "cwd");
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
    // извлечение — фоном: ждём карточку вложения в выдаче
    let t = Instant::now();
    let card = loop {
        let o = zam(&["search", "гидрант", "--json"], b"");
        let v: serde_json::Value = serde_json::from_str(&out(&o)).unwrap_or_default();
        if let Some(it) = v["items"]
            .as_array()
            .and_then(|a| a.iter().find(|i| i["kind"] == "attachment"))
        {
            break it.clone();
        }
        assert!(
            t.elapsed() < Duration::from_secs(90),
            "вложение не нашлось: {v}\n{}",
            err(&o)
        );
        std::thread::sleep(Duration::from_millis(300));
    };
    let chunk = card["id"].as_str().unwrap();
    let locator = card["locator"].as_str().unwrap();
    let b = format!("b:{}", card["blob_sha256"].as_str().unwrap());

    // a:<chunk> — фрагмент по locator чанка
    let o = zam(&["read", chunk], b"");
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let s = out(&o);
    assert!(s.contains("гидрант"), "{s}");
    assert!(s.contains("остатки.csv"), "{s}");
    // b:<sha> --locator
    let o = zam(&["read", &b, "--locator", locator], b"");
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains("вентиль"), "{}", out(&o));
    let o = zam(&["read", &b, "--locator", locator, "--json"], b"");
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let v: serde_json::Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(v["items"][0]["locator"], locator, "{v}");
    // без --locator — оглавление
    let o = zam(&["read", &b], b"");
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains(locator), "{}", out(&o));
    // locator, которого нет, — not_found (код 5)
    let o = zam(&["read", &b, "--locator", "sheet:Нет!A1:B2"], b"");
    assert_eq!(o.status.code(), Some(5), "{}\n{}", out(&o), err(&o));
    // --locator у заметки — 422 locator-invalid (код 6)
    let o = zam(&["read", note, "--locator", "page:1"], b"");
    assert_eq!(o.status.code(), Some(6), "{}\n{}", out(&o), err(&o));
    assert!(err(&o).contains("locator"), "{}", err(&o));
    // неверный формат — код 6
    let o = zam(&["read", &b, "--locator", "p.4"], b"");
    assert_eq!(o.status.code(), Some(6), "{}\n{}", out(&o), err(&o));
}
