//! `zam work` против настоящего zam-server в том же процессе тестов (M7.3 §6.3 п. 8): состояние
//! (`status`), пакет (`next`, только в сессии), итог из файла (`put`), возврат (`release`); старый
//! сервер без признака `works` — код 1 с подсказкой обновить `zam-server` (Review Focus 6).
//! Тестовый вид работ `echo` (сервер собран с `test-hooks`), модели нет.
mod common;

use std::future::Future;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Output;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use zam_server::config::ServerConfig;
use zam_server::{AppState, build};
use zam_store::Access;

/// Задание серверу в его рантайме: функция от состояния.
type Job = Box<dyn FnOnce(AppState) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>;

fn tmp(name: &str) -> PathBuf {
    common::sandbox("cli-work", name)
}

/// Сервер в фоновом потоке: адрес, токен `rw` пользователя «Анна» (режим исполнителя —
/// `agent`) и канал заданий в рантайм сервера.
struct Srv {
    url: String,
    tok: String,
    jobs: tokio::sync::mpsc::UnboundedSender<(Job, std::sync::mpsc::Sender<()>)>,
}

impl Srv {
    fn start(name: &str) -> Srv {
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
                st.system
                    .set_user_settings(u.id, vec![("works.mode".into(), json!("agent"))])
                    .await
                    .unwrap();
                st.user_flags.invalidate(u.id);
                let (jtx, mut jrx) =
                    tokio::sync::mpsc::unbounded_channel::<(Job, std::sync::mpsc::Sender<()>)>();
                let js = st.clone();
                tokio::spawn(async move {
                    while let Some((job, done)) = jrx.recv().await {
                        job(js.clone()).await;
                        let _ = done.send(());
                    }
                });
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let url = format!("http://{}", listener.local_addr().unwrap());
                tx.send((url, t, jtx)).unwrap();
                axum::serve(
                    listener,
                    app.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .await
                .unwrap();
            });
        });
        let (url, tok, jobs) = rx.recv().unwrap();
        Srv { url, tok, jobs }
    }

    /// Выполняет `f` в рантайме сервера и ждёт конца.
    fn run<F, Fut>(&self, f: F)
    where
        F: FnOnce(AppState) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let (tx, rx) = std::sync::mpsc::channel();
        let job: Job = Box::new(move |st| Box::pin(f(st)));
        self.jobs.send((job, tx)).unwrap();
        rx.recv_timeout(Duration::from_secs(60)).unwrap();
    }

    /// Ночь сегодняшней даты: постановка работ всех пользователей.
    fn night(&self) {
        self.run(|st| async move {
            let ctx = zam_server::works::NightCtx::new(st);
            let until = zam_store::time::now() + jiff::SignedDuration::from_hours(1);
            zam_server::works::queue::night(&ctx, zam_store::time::utc_today(), until)
                .await
                .unwrap();
        });
    }

    /// `zam` с адресом и токеном сервера в каталоге `dir`; `session` — `ZAM_SESSION`.
    fn z(&self, dir: &Path, session: &str, args: &[&str], stdin: &str) -> Output {
        common::zam_cmd()
            .env("ZAM_URL", &self.url)
            .env("ZAM_TOKEN", &self.tok)
            .env("ZAM_SESSION", session)
            .env("HOME", dir)
            .env("USERPROFILE", dir)
            .current_dir(dir)
            .args(args)
            .write_stdin(stdin.as_bytes().to_vec())
            .output()
            .unwrap()
    }
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

/// `zam work status` — режимы и очередь; `zam work next` в сессии — пакет (без сессии — отказ
/// сервера), `zam work put <файл>` — итог той же сессией, `--json` состояния — работа `done`;
/// возврат закрытой работы — код 4.
#[test]
fn work_status_next_put() {
    let s = Srv::start("flow");
    let dir = tmp("flow-repo");
    ok(&s.z(
        &dir,
        "-",
        &[
            "log",
            "insight",
            "--kind",
            "gotcha",
            "--title",
            "Для работы",
            "--scope",
            "global",
        ],
        "текст записи",
    ));
    s.night();
    let st = ok(&s.z(&dir, "-", &["work", "status"], ""));
    assert!(st.contains("echo agent"), "{st}");
    assert!(st.contains("очередь: 1"), "{st}");
    // без сессии пакет не выдаётся: итог принимает захватившая сессия
    let o = s.z(&dir, "-", &["work", "next"], "");
    assert_eq!(o.status.code(), Some(6), "{}{}", out(&o), err(&o));
    assert!(err(&o).contains("только в сессии"), "{}", err(&o));
    let pack = ok(&s.z(&dir, "cli-work-1", &["work", "next"], ""));
    assert!(pack.starts_with("## Внутренняя работа j:"), "{pack}");
    let work = pack
        .split_whitespace()
        .find(|w| w.starts_with("j:"))
        .unwrap()
        .to_string();
    assert!(pack.contains("<record n=\"1\""), "{pack}");
    let file = dir.join("r.json");
    std::fs::write(
        &file,
        json!({"work": work, "items": [{"n": 1, "text": "Для работы"}]}).to_string(),
    )
    .unwrap();
    let put = ok(&s.z(
        &dir,
        "cli-work-1",
        &["work", "put", "r.json", "--key", "k1"],
        "",
    ));
    assert!(
        put.starts_with(&format!("{work}: done — принято 1, отклонено 0")),
        "{put}"
    );
    // повтор с тем же ключом — прежний ответ
    let again = ok(&s.z(
        &dir,
        "cli-work-1",
        &["work", "put", "r.json", "--key", "k1"],
        "",
    ));
    assert_eq!(again, put);
    let v: Value =
        serde_json::from_str(&ok(&s.z(&dir, "-", &["work", "status", "--json"], ""))).unwrap();
    assert_eq!(v["works"][0]["state"], "done", "{v}");
    assert_eq!(v["works"][0]["id"], work.as_str(), "{v}");
    let o = s.z(&dir, "cli-work-1", &["work", "release", &work], "");
    assert_eq!(o.status.code(), Some(4), "{}{}", out(&o), err(&o));
}

/// Review Focus 6: `zam work` 0.0.16 с сервером 0.0.15 (нет признака `works`) — код 1 с
/// подсказкой обновить `zam-server` у каждой подкоманды.
#[test]
fn work_old_server_code_1() {
    let dir = tmp("old-repo");
    std::fs::write(dir.join("r.json"), r#"{"work": "j:abc", "items": []}"#).unwrap();
    let (url, _seen) = common::stub_with(Arc::new(|_: &str, p: &str, _: usize| {
        if p == "/api/v1/health" {
            (
                200,
                r#"{"status":"ok","features":["m6","agent_text_masked"]}"#,
                Duration::ZERO,
            )
        } else if p == "/api/v1/recall" {
            (
                400,
                r#"{"type":"about:blank","title":"Bad Request","status":400,"detail":"некорректное тело: unknown variant `work`"}"#,
                Duration::ZERO,
            )
        } else {
            (
                404,
                r#"{"type":"about:blank","title":"Not Found","status":404,"detail":"ресурс не найден"}"#,
                Duration::ZERO,
            )
        }
    }));
    for args in [
        vec!["work", "status"],
        vec!["work", "next"],
        vec!["work", "put", "r.json"],
        vec!["work", "release", "j:abc"],
    ] {
        let o = common::zam_cmd()
            .env("ZAM_URL", &url)
            .env("ZAM_TOKEN", "t")
            .env("ZAM_SESSION", "old-session")
            .env("HOME", &dir)
            .env("USERPROFILE", &dir)
            .current_dir(&dir)
            .args(&args)
            .write_stdin(Vec::new())
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(1), "{args:?}: {}{}", out(&o), err(&o));
        assert!(
            err(&o).contains("zam-server 0.0.16-m7.3"),
            "{args:?}: {}",
            err(&o)
        );
    }
}
