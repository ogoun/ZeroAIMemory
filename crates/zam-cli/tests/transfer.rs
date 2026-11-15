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
    let (url, tok, _) = start_state(name, tweak);
    (url, tok)
}

/// Как [`start_with`], плюс состояние сервера (аудит, задания).
fn start_state(
    name: &str,
    tweak: impl FnOnce(&mut ServerConfig) + Send + 'static,
) -> (String, String, zam_server::AppState) {
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
            tweak(&mut cfg);
            // место на диске раннера не влияет: свободное место подменено (1 ТиБ)
            zam_server::backup::test_hooks::set_free_space(&cfg.data_dir, Some(1 << 40));
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
            tx.send((url, t, st.clone())).unwrap();
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
            // ответы кончились — соединение закрывается без ответа, но слушатель жив: порт не
            // достаётся заглушке другого теста, куда попал бы повтор этого CLI
            let Some(next) = answers.next() else {
                continue;
            };
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

/// Что видела заглушка [`stub_early`].
#[derive(Default)]
struct Seen {
    /// Запрос импорта пришёл с `Expect: 100-continue`.
    expect: std::sync::atomic::AtomicBool,
    /// Запрос импорта пришёл с `Connection: close`.
    close: std::sync::atomic::AtomicBool,
    /// Журнал соединений (диагностика в сообщениях проверок).
    log: std::sync::Mutex<Vec<String>>,
}

impl Seen {
    fn expect(&self) -> bool {
        self.expect.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn close(&self) -> bool {
        self.close.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn log(&self) -> String {
        self.log.lock().unwrap().join("; ")
    }
}

/// Заглушка: на POST импорта читает заголовки; `head > 0` — отвечает `100 Continue` (как
/// сервер, начавший читать тело) и читает `head` байт тела; через `delay` отвечает `answer` и
/// закрывает свою сторону записи (как сервер после отказа: ureq 3.4.2 дочитывает ранний ответ
/// только до закрытия соединения), не дочитав тело, ждёт `hold` и рвёт соединение
/// (`shutdown` действует на сокет, даже если его описатель унаследовал параллельно запущенный
/// дочерний процесс). Прочие соединения закрываются. Слушатель живёт до конца процесса тестов
/// (порт не достаётся другой заглушке).
fn stub_early(answer: String, head: usize, delay: Duration, hold: Duration) -> (String, Arc<Seen>) {
    stub_early_with(answer, head, delay, hold, false)
}

/// Как [`stub_early`]; `keep_alive` — сервер с keep-alive: после ответа соединение не
/// закрывается (держится `hold`), если клиент не прислал `Connection: close` (тогда закрывается
/// сразу, как обязан сервер по RFC 9112).
fn stub_early_with(
    answer: String,
    head: usize,
    delay: Duration,
    hold: Duration,
    keep_alive: bool,
) -> (String, Arc<Seen>) {
    use std::io::{BufRead, BufReader, Read, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let seen = Arc::new(Seen::default());
    let s2 = seen.clone();
    std::thread::spawn(move || {
        let mut served = false;
        for c in l.incoming().flatten() {
            let t0 = std::time::Instant::now();
            let mut r = BufReader::new(c.try_clone().unwrap());
            let mut first = String::new();
            let _ = r.read_line(&mut first);
            let mut expect = false;
            let mut close = false;
            loop {
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                let low = line.to_ascii_lowercase();
                if low.starts_with("expect: 100-continue") {
                    expect = true;
                }
                if low.starts_with("connection: close") {
                    close = true;
                }
            }
            let import = first.starts_with("POST ") && first.contains("/import");
            s2.log
                .lock()
                .unwrap()
                .push(format!("{} expect={expect} close={close}", first.trim()));
            if !import || served {
                continue;
            }
            served = true;
            s2.expect.store(expect, std::sync::atomic::Ordering::SeqCst);
            s2.close.store(close, std::sync::atomic::Ordering::SeqCst);
            let mut c = c;
            if head > 0 {
                let _ = c.write_all(
                    b"HTTP/1.1 100 Continue

",
                );
                let mut body = vec![0u8; head];
                let _ = r.read_exact(&mut body);
            }
            std::thread::sleep(delay);
            // тело пошло до ответа: в буфере или в сокете уже есть байты
            let early = !r.buffer().is_empty() || {
                let _ = c.set_nonblocking(true);
                let mut one = [0u8; 1];
                let got = c.peek(&mut one).is_ok_and(|n| n > 0);
                let _ = c.set_nonblocking(false);
                got
            };
            s2.log.lock().unwrap().push(format!(
                "ответ через {} мс, тело до ответа: {early}",
                t0.elapsed().as_millis()
            ));
            let w = c.write_all(answer.as_bytes()).and_then(|()| c.flush());
            if !keep_alive || close {
                let _ = c.shutdown(std::net::Shutdown::Write);
            }
            s2.log.lock().unwrap().push(format!("запись {w:?}"));
            std::thread::spawn(move || {
                std::thread::sleep(hold);
                let _ = c.shutdown(std::net::Shutdown::Both);
            });
        }
    });
    (url, seen)
}

/// Плохо сжимаемый текст `n` байт (после deflate почти не меньше).
fn noise(n: usize) -> String {
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            char::from(b'a' + (x % 26) as u8)
        })
        .collect()
}

const ANSWER_503: &str = "HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/problem+json\r\n\
     Retry-After: 7\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}";

/// Каталог `big` с 32 МиБ плохо сжимаемого текста (тело больше дочитывания сервера).
fn big_repo(name: &str) -> PathBuf {
    let r = tmp(name);
    let src = r.join("big");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("big.md"), noise(32 << 20)).unwrap();
    r
}

/// «Ответ сервера важнее `BrokenPipe`»: сервер ответил 503 до тела (на `Expect:
/// 100-continue`, через 2 с — дольше 1 с ожидания ureq по умолчанию) и тело не читает —
/// упаковщик ловит `BrokenPipe`, но показывается ответ сервера (HTTP 503 с Retry-After), а не
/// обрыв приёма или упаковка.
#[test]
fn cli_import_server_answer_beats_broken_pipe_stub() {
    let (url, seen) = stub_early(
        ANSWER_503.to_string(),
        0,
        Duration::from_secs(2),
        Duration::from_secs(3),
    );
    let r = big_repo("imp-503-stub-repo");
    let t0 = std::time::Instant::now();
    let o = zam_idle(&url, &r, "30", &["import", "big", "--space", "knowledge"]);
    let e = format!("{} [{} мс]", stderr(&o), t0.elapsed().as_millis());
    assert!(seen.expect(), "нет Expect: {e}; {}", seen.log());
    assert!(e.contains("HTTP 503"), "{e}; {}", seen.log());
    assert!(e.contains("7"), "Retry-After: {e}");
    assert!(!e.contains("упаковка"), "{e}");
    assert!(!e.contains("прервал"), "{e}");
    assert!(!e.contains("ZipWriter"), "шум упаковщика: {e}");
}

/// Сервер прочёл несколько КиБ тела, ответил 503 и не дочитал: ureq читает ответ только после
/// всего тела, запись обрывается — CLI показывает подсказку об обрыве приёма, не упаковку.
#[test]
fn cli_import_server_cut_mid_body_hint_stub() {
    let (url, _) = stub_early(
        ANSWER_503.to_string(),
        8 << 10,
        Duration::ZERO,
        Duration::from_secs(3),
    );
    let r = big_repo("imp-503-cut-repo");
    let o = zam_idle(&url, &r, "30", &["import", "big", "--space", "knowledge"]);
    let e = stderr(&o);
    assert_eq!(o.status.code(), Some(2), "{e}");
    assert!(e.contains("сервер прервал приём архива"), "{e}");
    assert!(!e.contains("упаковка"), "{e}");
}

/// Настоящий сервер, отказ до приёма (507: точка `import_disk_full`), тело 32 МиБ больше
/// дочитывания: с `Expect` ответ приходит до тела — CLI показывает HTTP 507.
#[test]
fn cli_import_refusal_before_body_shown() {
    let (url, tok, st) = start_state("imp-507", |_| {});
    zam_server::backup::test_hooks::set_fail_point(&st.cfg.data_dir, Some("import_disk_full"));
    let r = big_repo("imp-507-repo");
    let o = zam(&url, &tok, &r, &["import", "big", "--space", "knowledge"]);
    zam_server::backup::test_hooks::set_fail_point(&st.cfg.data_dir, None);
    let e = stderr(&o);
    assert!(e.contains("HTTP 507"), "{e}");
    assert!(!e.contains("прервал"), "{e}");
}

/// Сбой упаковщика (файл каталога не открыть) — не нормальный конец тела: ureq обрывает
/// запрос, сервер не принимает задание (аудит `reason: client`), CLI показывает причину
/// упаковки.
#[test]
fn cli_import_pack_failure_aborts_body() {
    let (url, tok, st) = start_state("imp-packfail", |_| {});
    let r = tmp("imp-packfail-repo");
    let src = r.join("notes");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("a.md"), noise(4 << 20)).unwrap();
    let locked = src.join("z.md");
    std::fs::write(&locked, "закрытый").unwrap();
    // файл не открыть: Windows — без общего доступа, Unix — без прав чтения
    #[cfg(windows)]
    let _lock = {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&locked)
            .unwrap()
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::File::open(&locked).is_ok() {
            eprintln!("права не ограничивают чтение (root) — тест пропущен");
            return;
        }
    }
    let o = zam(&url, &tok, &r, &["import", "notes", "--space", "knowledge"]);
    let e = stderr(&o);
    assert_ne!(o.status.code(), Some(0), "{e}");
    assert!(e.contains("упаковка каталога"), "{e}");
    assert!(e.contains("z.md"), "{e}");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let t0 = std::time::Instant::now();
        loop {
            let rows = st.system.list_audit(100, None).await.unwrap();
            if let Some(d) = rows.iter().find(|r| r.action == "space.import.done") {
                assert_eq!(d.details["reason"], "client", "{:?}", d.details);
                break;
            }
            assert!(
                t0.elapsed() < Duration::from_secs(10),
                "нет аудита done: {:?}",
                rows.iter().map(|r| &r.action).collect::<Vec<_>>()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });
    assert_eq!(st.imports.running(), 0);
}

/// Загрузка `.zip` больше дочитывания (32 МиБ) при отказе до приёма: `Expect` и для файла —
/// ответ (HTTP 503 через 2 с) приходит до тела.
#[test]
fn cli_import_zip_answer_before_body_stub() {
    let (url, seen) = stub_early(
        ANSWER_503.to_string(),
        0,
        Duration::from_secs(2),
        Duration::from_secs(3),
    );
    let r = tmp("imp-zip-503-repo");
    std::fs::create_dir_all(&r).unwrap();
    std::fs::write(r.join("big.zip"), noise(32 << 20)).unwrap();
    let o = zam_idle(
        &url,
        &r,
        "30",
        &["import", "big.zip", "--space", "knowledge"],
    );
    let e = stderr(&o);
    assert!(seen.expect(), "нет Expect: {e}; {}", seen.log());
    assert!(e.contains("HTTP 503"), "{e}; {}", seen.log());
}

/// Прокси с keep-alive ответил рано (413) и сам соединение не закрывает: CLI шлёт
/// `Connection: close`, сервер закрывает соединение после ответа, CLI заканчивает сразу, а не
/// по сроку (ureq 3.4.2 дочитывает ранний ответ только до закрытия соединения).
#[test]
fn cli_import_early_answer_keep_alive_proxy_stub() {
    let (url, seen) = stub_early_with(
        "HTTP/1.1 413 Payload Too Large\r\nContent-Type: application/problem+json\r\n\
         Content-Length: 2\r\n\r\n{}"
            .to_string(),
        0,
        Duration::ZERO,
        Duration::from_secs(20),
        true,
    );
    let r = tmp("imp-keepalive-repo");
    std::fs::create_dir_all(&r).unwrap();
    std::fs::write(r.join("a.zip"), noise(1 << 20)).unwrap();
    let t0 = std::time::Instant::now();
    let o = zam_idle(&url, &r, "30", &["import", "a.zip", "--space", "knowledge"]);
    let ms = t0.elapsed().as_millis();
    let e = stderr(&o);
    assert!(seen.close(), "нет Connection: close: {e}; {}", seen.log());
    assert!(e.contains("HTTP 413"), "{e}; {}", seen.log());
    assert!(ms < 10_000, "CLI ждал закрытия соединения {ms} мс: {e}");
}

/// Каталог идёт с оценкой объёма: при малом свободном месте сервера (подмена: хватает на
/// оценку, но не на `2 × max_bytes`) маленький каталог принят; `.zip` — по `Content-Length`.
#[test]
fn cli_import_dir_estimate_fits_low_disk() {
    let (url, tok, st) = start_state("imp-est", |c| c.import.min_free_bytes = 0);
    zam_server::backup::test_hooks::set_free_space(&st.cfg.data_dir, Some(64 << 20));
    let r = tmp("imp-est-repo");
    let src = r.join("notes");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("a.md"), "# А\n\nтекст").unwrap();
    let o = zam(&url, &tok, &r, &["import", "notes", "--space", "knowledge"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(st.imports.disk_reserved(), 0);
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
