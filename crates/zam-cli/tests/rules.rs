//! `zam rules` (M7.3 §6.7 п. 2, задача 7): спонтанный кандидат в Rules из файла и список своих
//! кандидатов; сервер без признака `proposals` — код 1 с подсказкой обновить `zam-server`.
mod common;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use zam_server::build;
use zam_server::config::ServerConfig;
use zam_store::Access;

fn tmp(name: &str) -> PathBuf {
    common::sandbox("cli-rules", name)
}

/// Сервер в фоновом потоке: пользователь с токеном `rw`; адрес и токен.
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
        .env("ZAM_SESSION", "cli-rules-session")
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

const REASON: &str = "Правило поведения агента во всех проектах";

/// §6.7 п. 2: `zam rules candidate <путь> --file F --reason R --basis ID` → `o:<id>` (автор —
/// сессия `ZAM_SESSION`); повтор того же пути и текста — прежнее предложение; `zam rules
/// candidates` — свои кандидаты (текстом и `--json`); текст с URL — ошибка сервера (код 4).
#[test]
fn rules_candidate_and_list() {
    let (url, tok) = start("list");
    let r = tmp("list-repo");
    let f = r.join("rule.md");
    std::fs::write(
        &f,
        "Перед ответом человеку записывай итог задачи в память.\n",
    )
    .unwrap();
    let fs = f.to_str().unwrap();
    let basis = format!("i:{}", zam_core::Id::new());
    let out = ok(&zam(
        &url,
        &tok,
        &r,
        &[
            "rules",
            "candidate",
            "rules:/process/summary",
            "--file",
            fs,
            "--reason",
            REASON,
            "--basis",
            &basis,
        ],
    ));
    assert!(
        out.starts_with("o:") && out.contains("решает человек"),
        "{out}"
    );
    let o = out.split_whitespace().next().unwrap().to_string();
    let again = ok(&zam(
        &url,
        &tok,
        &r,
        &[
            "rules",
            "candidate",
            "process/summary",
            "--file",
            fs,
            "--reason",
            REASON,
        ],
    ));
    assert!(
        again.starts_with(&o) && again.contains("уже есть (proposed)"),
        "{again}"
    );
    let list = ok(&zam(&url, &tok, &r, &["rules", "candidates"]));
    assert!(
        list.contains(&format!(
            "{o} [proposed] rules:/process/summary · Перед ответом"
        )),
        "{list}"
    );
    let v: Value = serde_json::from_str(&ok(&zam(
        &url,
        &tok,
        &r,
        &["rules", "candidates", "--json"],
    )))
    .unwrap();
    let p = &v["proposals"][0];
    assert_eq!(p["id"], o.as_str());
    assert_eq!(p["session"], "cli-rules-session");
    assert_eq!(p["basis"][0], basis.as_str());
    let none = ok(&zam(
        &url,
        &tok,
        &r,
        &["rules", "candidates", "--status", "declined"],
    ));
    assert!(none.contains("кандидатов нет"), "{none}");
    std::fs::write(&f, "Сверяйся с https://example.com перед выпуском.\n").unwrap();
    let bad = zam(
        &url,
        &tok,
        &r,
        &[
            "rules",
            "candidate",
            "process/url",
            "--file",
            fs,
            "--reason",
            REASON,
        ],
    );
    assert_ne!(bad.status.code(), Some(0), "{bad:?}");
    assert!(
        String::from_utf8_lossy(&bad.stderr).contains("URL"),
        "{bad:?}"
    );
    // чужое пространство — ошибка ввода до запроса
    let bad = zam(
        &url,
        &tok,
        &r,
        &[
            "rules",
            "candidate",
            "knowledge:/x",
            "--file",
            fs,
            "--reason",
            REASON,
        ],
    );
    assert_ne!(bad.status.code(), Some(0), "{bad:?}");
}

/// Разные версии (Review Focus 6): сервер 0.0.15 без `/proposals` (404) и без `proposals` в
/// `/health.features` — код 1 с подсказкой обновить `zam-server` у `candidate` и `candidates`.
#[test]
fn rules_candidate_old_server_code_1() {
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
    let f = r.join("rule.md");
    std::fs::write(&f, "Правило.\n").unwrap();
    let fs = f.to_str().unwrap();
    for args in [
        &[
            "rules",
            "candidate",
            "a/b",
            "--file",
            fs,
            "--reason",
            REASON,
        ][..],
        &["rules", "candidates"][..],
    ] {
        let o = zam(&url, "t", &r, args);
        assert_eq!(o.status.code(), Some(1), "{args:?}: {o:?}");
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(
            err.contains("сервер не поддерживает кандидаты в Rules")
                && err.contains("обновите zam-server"),
            "{err}"
        );
    }
}
