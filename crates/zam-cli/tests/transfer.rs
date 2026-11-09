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
