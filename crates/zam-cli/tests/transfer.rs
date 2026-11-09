//! `zam export` (M6 §12б.5, задача 18b1): архив в файл через `<файл>.part`, сервер без маршрута
//! экспорта (404 на существующее пространство), несуществующее пространство, оборванный поток.
mod common;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::Arc;
use std::time::Duration;

use zam_server::build;
use zam_server::config::ServerConfig;
use zam_store::{
    Access, Attribution, AuthorKind, NewNote, NodePath, SpaceKind, create_note, space_of_kind,
};

fn tmp(name: &str) -> PathBuf {
    common::sandbox("cli-transfer", name)
}

fn zam(url: &str, token: &str, cwd: &Path, args: &[&str]) -> Output {
    common::zam_cmd()
        .env("ZAM_URL", url)
        .env("ZAM_TOKEN", token)
        .env("HOME", cwd)
        .env("USERPROFILE", cwd)
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// Сервер в фоновом потоке с заметками `docs/a` и `other` в knowledge пользователя: адрес, токен.
fn start(name: &str) -> (String, String) {
    start_with(name, |_| {})
}

/// [`start`] с правкой конфигурации сервера.
fn start_with(
    name: &str,
    tweak: impl FnOnce(&mut ServerConfig) + Send + 'static,
) -> (String, String) {
    let data = tmp(&format!("{name}-data"));
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
            // запас 5 % диска на почти полном диске разработчика отверг бы импорт
            cfg.import.min_free_percent = 0;
            tweak(&mut cfg);
            let (app, st, _) = build(cfg, Some("pw".into())).await.unwrap();
            let u = st.system.create_user("Анна", false).await.unwrap();
            let (_, t) = st.system.issue_token(u.id, "t", Access::Rw).await.unwrap();
            let db = st.users.get(u.id).await.unwrap();
            let k = space_of_kind(&db, SpaceKind::Knowledge).await.unwrap();
            for (p, text) in [("docs/a", "EXPORT-DOC-A"), ("other", "EXPORT-OTHER")] {
                create_note(
                    &db,
                    NewNote {
                        space_id: k.id,
                        path: NodePath::parse(p).unwrap(),
                        content: text.into(),
                        l0: None,
                        attrib: Attribution::new(AuthorKind::Human),
                    },
                )
                .await
                .unwrap();
            }
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

/// Файлы `*.part` каталога.
fn parts(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".part"))
        .collect()
}

fn has(hay: &[u8], needle: &str) -> bool {
    hay.windows(needle.len()).any(|w| w == needle.as_bytes())
}

/// Архив пишется в файл: zip с описью, фильтр `--path` доходит до сервера, `.part` не остаётся;
/// `--json` — путь и размер.
#[test]
fn cli_export_writes_zip() {
    let (url, tok) = start("ok");
    let r = tmp("ok-repo");
    let o = zam(
        &url,
        &tok,
        &r,
        &[
            "export",
            "--space",
            "knowledge",
            "--path",
            "docs",
            "-o",
            "out.zip",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let b = std::fs::read(r.join("out.zip")).unwrap();
    assert!(b.starts_with(b"PK"));
    assert!(has(&b, "manifest.json"));
    assert!(has(&b, "docs/a.md"));
    assert!(!has(&b, "other.md"), "фильтр --path не применён");
    assert!(parts(&r).is_empty(), "{:?}", parts(&r));
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    assert!(out.contains(&format!("({} байт)", b.len())), "{out}");

    let o = zam(
        &url,
        &tok,
        &r,
        &["export", "-o", "all.zip", "--no-history", "--json"],
    );
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["space"], "knowledge");
    let b = std::fs::read(r.join("all.zip")).unwrap();
    assert_eq!(v["bytes"], b.len());
    assert!(has(&b, "other.md") && !has(&b, "_history/"));
}

/// Сервер до 0.0.11-m6: маршрута экспорта нет (404), а пространство есть — «сервер не
/// поддерживает экспорт», код 1; файл не создаётся.
#[test]
fn cli_export_old_server_404_message() {
    let (url, seen) = common::stub_with(Arc::new(|_: &str, p: &str, _: usize| {
        if p.contains("/tree") {
            (200, r#"{"space":{},"items":[]}"#, Duration::ZERO)
        } else {
            (
                404,
                r#"{"type":"about:blank","title":"Not Found","status":404,"detail":"ресурс не найден"}"#,
                Duration::ZERO,
            )
        }
    }));
    let r = tmp("old-repo");
    let o = zam(&url, "t", &r, &["export", "-o", "a.zip"]);
    assert_eq!(o.status.code(), Some(1), "{o:?}");
    assert!(
        stderr(&o).contains("сервер не поддерживает экспорт пространства"),
        "{}",
        stderr(&o)
    );
    assert!(!r.join("a.zip").exists());
    assert!(parts(&r).is_empty(), "{:?}", parts(&r));
    let s = seen.lock().unwrap();
    assert!(
        s[0].1.starts_with("/api/v1/spaces/knowledge/export"),
        "{s:?}"
    );
    assert!(s[1].1.starts_with("/api/v1/spaces/knowledge/tree"), "{s:?}");
}

/// Нет пространства (404 и у дерева) — обычное «не найдено», код 5.
#[test]
fn cli_export_missing_space_not_found() {
    let (url, _) = common::stub(
        404,
        r#"{"type":"about:blank","title":"Not Found","status":404,"detail":"ресурс не найден"}"#,
        Duration::ZERO,
    );
    let r = tmp("missing-repo");
    let o = zam(
        &url,
        "t",
        &r,
        &[
            "export",
            "--space",
            "01JAAAAAAAAAAAAAAAAAAAAAAA",
            "-o",
            "a.zip",
        ],
    );
    assert_eq!(o.status.code(), Some(5), "{o:?}");
    assert!(!stderr(&o).contains("не поддерживает"), "{}", stderr(&o));
}

/// Тело оборвано (нет конца каталога zip) — код 2, своего `.part` нет, прежний файл цели и
/// чужой `a.zip.part` целы.
#[test]
fn cli_export_truncated_stream_keeps_target() {
    let (url, _) = common::stub(200, "PK\u{3}\u{4}обрыв", Duration::ZERO);
    let r = tmp("trunc-repo");
    std::fs::write(r.join("a.zip"), b"old").unwrap();
    std::fs::write(r.join("a.zip.part"), b"foreign").unwrap();
    let o = zam(&url, "t", &r, &["export", "-o", "a.zip"]);
    assert_eq!(o.status.code(), Some(2), "{o:?}");
    assert!(stderr(&o).contains("архив не завершён"), "{}", stderr(&o));
    assert_eq!(std::fs::read(r.join("a.zip")).unwrap(), b"old");
    assert_eq!(std::fs::read(r.join("a.zip.part")).unwrap(), b"foreign");
    assert_eq!(parts(&r), vec!["a.zip.part"]);
}

/// Конец вложенного zip (несжатый `.docx` в конце обрезанного потока) — не конец архива: код 2,
/// цели нет.
#[test]
fn cli_export_inner_zip_tail_rejected() {
    // локальный заголовок внешнего архива, затем «docx»: пустой zip из одной записи конца
    let (url, _) = common::stub(
        200,
        "PK\u{3}\u{4}outer-header-docx-data:PK\u{5}\u{6}\u{0}\u{0}\u{0}\u{0}\u{0}\u{0}\u{0}\u{0}\u{0}\u{0}\u{0}\u{0}\u{0}\u{0}\u{0}\u{0}\u{0}\u{0}",
        Duration::ZERO,
    );
    let r = tmp("inner-repo");
    let o = zam(&url, "t", &r, &["export", "-o", "a.zip"]);
    assert_eq!(o.status.code(), Some(2), "{o:?}");
    assert!(stderr(&o).contains("архив не завершён"), "{}", stderr(&o));
    assert!(!r.join("a.zip").exists());
    assert!(parts(&r).is_empty(), "{:?}", parts(&r));
}

/// Сервер прислал заголовки и часть тела и замолчал (полуоткрытое соединение): через
/// `ZAM_EXPORT_IDLE_SECS` — код 2, свой `.part` удалён, цели нет.
#[test]
fn cli_export_idle_watchdog() {
    use std::io::{Read, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    std::thread::spawn(move || {
        for mut c in l.incoming().flatten() {
            let mut buf = [0u8; 4096];
            let _ = c.read(&mut buf);
            let _ = c.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/zip\r\n\
                  Transfer-Encoding: chunked\r\n\r\n4\r\nPK\x03\x04\r\n",
            );
            let _ = c.flush();
            // молчим, соединение открыто
            std::thread::sleep(Duration::from_secs(60));
        }
    });
    let r = tmp("idle-repo");
    let t0 = std::time::Instant::now();
    let o = common::zam_cmd()
        .env("ZAM_URL", &url)
        .env("ZAM_TOKEN", "t")
        .env("HOME", &r)
        .env("USERPROFILE", &r)
        .env("ZAM_EXPORT_IDLE_SECS", "1")
        .current_dir(&r)
        .args(["export", "-o", "a.zip"])
        .timeout(Duration::from_secs(30))
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2), "{o:?}");
    assert!(t0.elapsed() < Duration::from_secs(20), "{:?}", t0.elapsed());
    assert!(stderr(&o).contains("сервер молчит"), "{}", stderr(&o));
    assert!(!r.join("a.zip").exists());
    assert!(parts(&r).is_empty(), "{:?}", parts(&r));
}

/// Каталог без описи — «папка `.md`»: архив чужой (новые id, `axiom` → `theorem`), упаковка —
/// потоком без временного файла; задание опрашивается до итога; повтор — пропуск.
#[test]
fn cli_import_folder_md() {
    let (url, tok) = start("imp-dir");
    let r = tmp("imp-dir-repo");
    let src = r.join("notes");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(
        src.join("rule.md"),
        "---\ncertainty: axiom\n---\n# Правило\n\nLF везде\n",
    )
    .unwrap();
    std::fs::write(src.join("sub").join("b.md"), "# B\n\nтекст\n").unwrap();
    std::fs::write(src.join("pic.png"), b"\x89PNG").unwrap();
    let o = zam(
        &url,
        &tok,
        &r,
        &["import", "notes", "--space", "knowledge", "--json"],
    );
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let rep: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(rep["created"], 2, "{rep}");
    assert_eq!(rep["demoted_axioms"], 1, "{rep}");
    assert_eq!(rep["own"], false, "{rep}");
    assert!(parts(&r).is_empty(), "{:?}", parts(&r));
    let o = zam(&url, &tok, &r, &["read", "knowledge:rule"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(
        has(&o.stdout, "certainty: theorem"),
        "{}",
        String::from_utf8_lossy(&o.stdout)
    );
    let o = zam(
        &url,
        &tok,
        &r,
        &["import", "notes", "--space", "knowledge", "--json"],
    );
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let rep: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    // повтор «папки .md» идемпотентен: та же заметка по пути и хэшу
    assert_eq!(rep["created"], 0, "{rep}");
    assert_eq!(rep["skipped"], 2, "{rep}");
    assert_eq!(rep["id_remapped"], 0, "{rep}");
    assert!(rep["conflicts"].as_array().unwrap().is_empty(), "{rep}");
    assert!(stderr(&o).contains("не отменяется"), "{}", stderr(&o));
}

/// Заглушка сервера: на каждое соединение — следующий ответ списка (`None` — закрыть соединение
/// без ответа: сетевой сбой); тело запроса дочитывается по `Content-Length`.
fn stub(answers: Vec<Option<String>>) -> String {
    use std::io::{BufRead, BufReader, Read, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    std::thread::spawn(move || {
        let mut answers = answers.into_iter();
        for c in l.incoming().flatten() {
            let mut r = BufReader::new(c.try_clone().unwrap());
            let mut len = 0usize;
            loop {
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; len];
            let _ = r.read_exact(&mut body);
            let Some(next) = answers.next() else { return };
            let mut c = c;
            match next {
                None => drop(c),
                Some(a) => {
                    let _ = c.write_all(a.as_bytes());
                    let _ = c.flush();
                }
            }
        }
    });
    url
}

fn http(status: &str, body: &str) -> Option<String> {
    Some(format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    ))
}

const ACCEPTED: &str = r#"{"job":"01J00000000000000000000000","status":"/spaces/knowledge/imports/01J00000000000000000000000"}"#;
const DONE: &str = r#"{"state":"done","stage":"done","queue":null,"progress":{"total":1,"done":1},"report":{"created":1,"revised":0,"skipped":0,"conflicts":[],"demoted_axioms":0,"id_remapped":0,"links_skipped":0,"warnings":[],"unresolved":[],"dry_run":false}}"#;

fn zip_file(r: &Path) -> PathBuf {
    let p = r.join("a.zip");
    std::fs::write(&p, b"PK\x05\x06\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0").unwrap();
    p
}

fn zam_idle(url: &str, cwd: &Path, idle: &str, args: &[&str]) -> Output {
    common::zam_cmd()
        .env("ZAM_URL", url)
        .env("ZAM_TOKEN", "t")
        .env("HOME", cwd)
        .env("USERPROFILE", cwd)
        .env("ZAM_IMPORT_IDLE_SECS", idle)
        .current_dir(cwd)
        .args(args)
        .timeout(Duration::from_secs(60))
        .output()
        .unwrap()
}

/// Задание пропало (сервер перезапущен, опрос — 404): понятное сообщение, код 2.
#[test]
fn cli_import_job_lost_404() {
    let url = stub(vec![
        http("202 Accepted", ACCEPTED),
        http("404 Not Found", r#"{"status":404,"detail":"не найдено"}"#),
    ]);
    let r = tmp("imp-lost");
    zip_file(&r);
    let o = zam_idle(&url, &r, "30", &["import", "a.zip", "--space", "knowledge"]);
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
    assert!(stderr(&o).contains("сервер перезапущен"), "{}", stderr(&o));
}

/// Сетевой сбой опроса — повтор в пределах сторожа, итог получен.
#[test]
fn cli_import_poll_retries_network() {
    let url = stub(vec![
        http("202 Accepted", ACCEPTED),
        None,
        http("200 OK", DONE),
    ]);
    let r = tmp("imp-retry");
    zip_file(&r);
    let o = zam_idle(
        &url,
        &r,
        "30",
        &["import", "a.zip", "--space", "knowledge", "--json"],
    );
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let rep: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(rep["created"], 1);
}

/// Ожидание в очереди — не простой: сторож 1 с, задание три опроса в очереди, итог получен;
/// место в очереди напечатано.
#[test]
fn cli_import_queued_is_not_idle() {
    let queued =
        r#"{"state":"running","stage":"queued","queue":2,"progress":{"total":0,"done":0}}"#;
    let url = stub(vec![
        http("202 Accepted", ACCEPTED),
        http("200 OK", queued),
        http("200 OK", queued),
        http("200 OK", queued),
        http("200 OK", DONE),
    ]);
    let r = tmp("imp-queued");
    zip_file(&r);
    let o = zam_idle(
        &url,
        &r,
        "1",
        &["import", "a.zip", "--space", "knowledge", "--json"],
    );
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(
        stderr(&o).contains("в очереди, впереди заданий: 2"),
        "{}",
        stderr(&o)
    );
}

/// Сервер отверг тело (413), пока упаковщик ещё пишет каталог (`BrokenPipe`): показывается
/// ошибка запроса, а не упаковки.
#[test]
fn cli_import_http_error_beats_broken_pipe() {
    let (url, tok) = start_with("imp-413", |c| c.import.max_bytes = 1 << 20);
    let r = tmp("imp-413-repo");
    let src = r.join("big");
    std::fs::create_dir_all(&src).unwrap();
    // плохо сжимаемый текст: после deflate больше 1 МиБ
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let text: String = (0..(4 << 20))
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            char::from(b'a' + (x % 26) as u8)
        })
        .collect();
    std::fs::write(src.join("big.md"), text).unwrap();
    let o = zam(&url, &tok, &r, &["import", "big", "--space", "knowledge"]);
    assert_eq!(o.status.code(), Some(6), "{}", stderr(&o));
    assert!(stderr(&o).contains("HTTP 413"), "{}", stderr(&o));
    assert!(!stderr(&o).contains("упаковка"), "{}", stderr(&o));
}

/// Две разные папки с одинаковыми файлами не делят id: вторая — конфликт `path-taken`, а не
/// «та же заметка»; повтор первой — пропуск без переназначений.
#[test]
fn cli_import_two_folders_distinct_ids() {
    let (url, tok) = start("imp-2dirs");
    let r = tmp("imp-2dirs-repo");
    for d in ["one", "two"] {
        std::fs::create_dir_all(r.join(d)).unwrap();
        std::fs::write(r.join(d).join("x.md"), "# X\n\nодинаково\n").unwrap();
    }
    let run = |d: &str| {
        let o = zam(
            &url,
            &tok,
            &r,
            &["import", d, "--space", "knowledge", "--json"],
        );
        assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
        serde_json::from_slice::<serde_json::Value>(&o.stdout).unwrap()
    };
    assert_eq!(run("one")["created"], 1);
    let two = run("two");
    assert_eq!(two["created"], 0, "{two}");
    assert_eq!(two["skipped"], 0, "{two}");
    assert_eq!(two["conflicts"][0]["reason"], "path-taken", "{two}");
    let again = run("one");
    assert_eq!(again["skipped"], 1, "{again}");
    assert_eq!(again["id_remapped"], 0, "{again}");
}
