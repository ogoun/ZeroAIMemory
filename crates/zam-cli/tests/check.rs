//! `zam check` (M7.3 §6.5 п. 7, задача 5): ручной прогон с ожиданием, `status`, `report --json`;
//! сервер без признака `memory_check` — код 1 с подсказкой обновить `zam-server`.
mod common;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use zam_server::build;
use zam_server::config::ServerConfig;
use zam_store::probes::{NewProbe, ProbeAuthor, ProbeKind, ProbeTarget, Split};
use zam_store::{Access, SpaceKind};

fn tmp(name: &str) -> PathBuf {
    common::sandbox("cli-check", name)
}

/// Сервер в фоновом потоке: пользователь с заметкой Knowledge и пробой о ней; адрес и токен.
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
                ..common::server_defaults()
            };
            let (app, st, _) = build(cfg, Some("pw".into())).await.unwrap();
            let u = st.system.create_user("Анна", false).await.unwrap();
            let (_, t) = st.system.issue_token(u.id, "t", Access::Rw).await.unwrap();
            let db = st.users.get(u.id).await.unwrap();
            let kn = zam_store::list_spaces(&db)
                .await
                .unwrap()
                .into_iter()
                .find(|s| s.kind == SpaceKind::Knowledge)
                .unwrap()
                .id;
            let node = zam_store::create_note(
                &db,
                zam_store::NewNote {
                    space_id: kn,
                    path: zam_store::NodePath::parse("kb/деплой.md").unwrap(),
                    content: "# Деплой\n\nnovemberkilo выкатываем ночью.\n".into(),
                    l0: None,
                    attrib: zam_store::Attribution::new(zam_store::AuthorKind::Human),
                },
            )
            .await
            .unwrap();
            zam_store::probes::insert_probe(
                &db,
                NewProbe {
                    space: Some(kn),
                    question: "когда выкатываем novemberkilo".into(),
                    kind: ProbeKind::Probe,
                    targets: vec![ProbeTarget {
                        target: format!("n:{}", node.id),
                        rev: 1,
                    }],
                    split: Split::Test,
                    overlap: Some(0.0),
                    author_kind: ProbeAuthor::Agent,
                    author: None,
                    work_id: None,
                },
            )
            .await
            .unwrap();
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

fn zam(url: &str, token: &str, cwd: &Path, args: &[&str]) -> Output {
    let mut c = common::zam_cmd();
    c.env("ZAM_URL", url)
        .env("ZAM_TOKEN", token)
        .env("HOME", cwd)
        .env("USERPROFILE", cwd)
        .current_dir(cwd)
        .args(args);
    c.output().unwrap()
}

fn ok(o: &Output) -> String {
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    assert_eq!(
        o.status.code(),
        Some(0),
        "stdout: {out}\nstderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    out
}

/// §6.5 п. 7: `zam check run` ждёт конца ручного прогона; `zam check report --json` — ответ
/// `GET /check/status` (последний прогон с вердиктом и метриками, тренд, пробы); текстовый
/// отчёт и `status` — по-русски, время UTC.
#[test]
fn check_report_json() {
    let (url, tok) = start("report");
    let r = tmp("report-repo");
    let out = ok(&zam(&url, &tok, &r, &["check", "run"]));
    assert!(out.contains("подробно — zam check report"), "{out}");
    let v: Value =
        serde_json::from_str(&ok(&zam(&url, &tok, &r, &["check", "report", "--json"]))).unwrap();
    assert_eq!(v["last"]["status"], "done", "{v}");
    assert_eq!(v["last"]["trigger"], "manual");
    assert!(v["last"]["verdict"]["kind"].is_string(), "{v}");
    assert_eq!(v["last"]["verdict"]["base_of"], 5);
    assert_eq!(v["last"]["metrics"]["all"]["n"], 1);
    assert_eq!(v["trend"].as_array().unwrap().len(), 1);
    assert_eq!(v["probes"]["by_kind"]["probe"], 1);
    assert!(v["failing"].is_array() && v["advice"].is_array());
    let text = ok(&zam(&url, &tok, &r, &["check", "report"]));
    assert!(
        text.contains("вердикт:") && text.contains("hit_search"),
        "{text}"
    );
    let st = ok(&zam(&url, &tok, &r, &["check", "status"]));
    assert!(
        st.contains("последний прогон") && st.contains("ночная проверка: сервер off"),
        "{st}"
    );
    // счётчики проб — словами, а не JSON
    assert!(st.contains("виды: negative 30, probe 1; части: "), "{st}");
    let no_wait: Value = serde_json::from_str(&ok(&zam(
        &url,
        &tok,
        &r,
        &["check", "run", "--no-wait", "--json"],
    )))
    .unwrap();
    assert!(no_wait["run"].is_string(), "{no_wait}");
}

/// Разные версии (Review Focus 6): сервер 0.0.15 без `/check` (404) и без `memory_check` в
/// `/health.features` — код 1 с подсказкой обновить `zam-server` у `status`, `report`, `run`.
#[test]
fn check_old_server_code_1() {
    let not_found =
        r#"{"type":"about:blank","title":"Not Found","status":404,"detail":"ресурс не найден"}"#;
    let (url, _) = common::stub_with(Arc::new(move |_: &str, p: &str, _: usize| {
        if p == "/api/v1/health" {
            (
                200,
                r#"{"status":"ok","version":"0.0.15-m7.2","features":["m6","agent_text_masked"]}"#,
                Duration::ZERO,
            )
        } else {
            (404, not_found, Duration::ZERO)
        }
    }));
    let r = tmp("old-repo");
    for args in [
        &["check", "status"][..],
        &["check", "report", "--json"][..],
        &["check", "run"][..],
    ] {
        let o = zam(&url, "t", &r, args);
        assert_eq!(o.status.code(), Some(1), "{args:?}: {o:?}");
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(
            err.contains("сервер не поддерживает проверку памяти")
                && err.contains("обновите zam-server"),
            "{err}"
        );
    }
}
