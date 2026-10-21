//! Команды `zam-server qdrant fetch|status|reset` (задача 6 M2) без настоящего Qdrant:
//! загрузка — с локального HTTP-сервера теста, файлы — под `CARGO_TARGET_TMPDIR`.

mod common;

use std::io::{Read as _, Write as _};
use std::path::Path;

use common::{cfg_in, tmp};
use zam_index::supervisor::{QDRANT_VERSION, qdrant_exe_name};
use zam_server::qdrant;

/// zip с исполняемым файлом Qdrant в корне (как в релизе Windows).
fn make_zip(path: &Path, body: &[u8]) {
    let mut z = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    z.start_file(qdrant_exe_name(), zip::write::SimpleFileOptions::default())
        .unwrap();
    z.write_all(body).unwrap();
    z.finish().unwrap();
}

/// Отдаёт `body` на один запрос; возвращает URL.
fn serve_once(body: Vec<u8>) -> String {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/q.zip", l.local_addr().unwrap());
    std::thread::spawn(move || {
        let Ok((mut s, _)) = l.accept() else { return };
        let mut req = Vec::new();
        let mut b = [0u8; 1024];
        while !req.windows(4).any(|w| w == b"\r\n\r\n") {
            match s.read(&mut b) {
                Ok(0) | Err(_) => break,
                Ok(k) => req.extend_from_slice(&b[..k]),
            }
        }
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = s.write_all(head.as_bytes());
        let _ = s.write_all(&body);
    });
    url
}

fn manifest(version: &str, url: &str, sha: &str) -> String {
    let exe = qdrant_exe_name();
    format!(
        "name = \"qdrant\"\nversion = \"{version}\"\n\
         [platform.windows-x64]\nurl = \"{url}\"\nsha256 = \"{sha}\"\nextract = [\"{exe}\"]\n\
         [platform.linux-x64]\nurl = \"{url}\"\nsha256 = \"{sha}\"\nextract = [\"{exe}\"]\n"
    )
}

#[test]
fn fetch_installs_binary_once_and_checks_version_and_url() {
    let d = tmp("qdrant-fetch");
    let zip_p = d.join("src.zip");
    make_zip(&zip_p, b"QDRANT");
    let sha = zam_fetch::sha256_file(&zip_p).unwrap();
    let url = serve_once(std::fs::read(&zip_p).unwrap());
    let mpath = d.join("qdrant.toml");
    std::fs::write(&mpath, manifest(QDRANT_VERSION, &url, &sha)).unwrap();

    let m = qdrant::load_manifest(Some(&mpath)).unwrap();
    let binary = zam_index::supervisor::default_binary(&d.join("inst"));
    let report = qdrant::fetch(&m, &binary).unwrap();
    assert!(report.contains("установлен"), "{report}");
    assert_eq!(std::fs::read(&binary).unwrap(), b"QDRANT");
    // Уже на месте — без сети (сервер теста обслужил единственный запрос).
    let again = qdrant::fetch(&m, &binary).unwrap();
    assert!(again.contains("уже установлен"), "{again}");

    // Встроенный манифест — закреплённой версии.
    assert_eq!(qdrant::load_manifest(None).unwrap().version, QDRANT_VERSION);
    // Манифест другой версии сервер не примет.
    std::fs::write(&mpath, manifest("1.0.0", &url, &sha)).unwrap();
    let e = qdrant::load_manifest(Some(&mpath)).unwrap_err().to_string();
    assert!(e.contains("1.0.0"), "{e}");
    // Только https (кроме локальной петли).
    std::fs::write(
        &mpath,
        manifest(QDRANT_VERSION, "http://example.com/q.zip", &sha),
    )
    .unwrap();
    let m = qdrant::load_manifest(Some(&mpath)).unwrap();
    let e = qdrant::fetch(&m, &d.join("other").join(qdrant_exe_name()))
        .unwrap_err()
        .to_string();
    assert!(e.contains("https"), "{e}");
}

#[tokio::test]
async fn status_and_reset_on_stopped_install() {
    let d = tmp("qdrant-status");
    let cfg = cfg_in(d.clone());
    let s = qdrant::status(&cfg, &d.join("inst")).await.unwrap();
    for needle in [
        "режим: managed",
        "нет — zam-server qdrant fetch",
        "лок: свободен",
        "pid: нет",
        QDRANT_VERSION,
    ] {
        assert!(s.contains(needle), "нет `{needle}` в\n{s}");
    }
    let j = qdrant::status_json(&cfg, &d.join("inst")).await.unwrap();
    assert_eq!(j["mode"], "managed");
    assert_eq!(j["binary_present"], false);
    assert_eq!(j["locked"], false);
    assert!(j["pid"].is_null());
    assert_eq!(j["pid_alive"], false);
    assert_eq!(j["pinned_version"], QDRANT_VERSION);
    assert!(j["running_version"].is_null());

    let e = qdrant::reset(&cfg, &d.join("inst"), false)
        .unwrap_err()
        .to_string();
    assert!(e.contains("--yes"), "{e}");
    std::fs::create_dir_all(d.join("qdrant").join("storage")).unwrap();
    qdrant::reset(&cfg, &d.join("inst"), true).unwrap();
    assert!(!d.join("qdrant").join("storage").exists());
    let s = qdrant::status(&cfg, &d.join("inst")).await.unwrap();
    assert!(s.contains("пересборка индекса: требуется"), "{s}");
}
