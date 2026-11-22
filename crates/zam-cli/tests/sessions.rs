//! `zam sessions tokens` против настоящего zam-server в том же процессе тестов (M6 §8, задача
//! 8b): таблица «событие · вызовов · токенов · уже выдавалось» одной сессии, сумма по
//! `--last N`, `--by-holdout`, `--json` и сессия агента из `ZAM_SESSION`; выдача без сессии
//! (`--unattributed`, строка «без сессии за 30 сут.») и субагенты сессии (`--subagents`) — M7
//! §5.8 п. 2 (задача 9 M7.2).
mod common;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::Value;

use zam_server::build;
use zam_server::config::ServerConfig;
use zam_store::{Access, SessionTouch, TokenCount};

fn tmp(name: &str) -> PathBuf {
    common::sandbox("cli-sessions", name)
}

fn count(event: &str, calls: u64, tokens: u64, reread: u64) -> TokenCount {
    TokenCount {
        event: event.into(),
        calls,
        tokens,
        reread_calls: reread,
    }
}

/// Выдача без сессии для посева: (суток назад, событие, вызовов, токенов).
type Unattributed = &'static [(i64, &'static str, u32, u64)];

/// Что ещё посеять в базе пользователя тестового сервера ([`start_with`]).
#[derive(Default, Clone, Copy)]
struct Seed {
    /// Субагенты `s-new:a1` (`pre_tool` 2/20, `read` 1/50) и `s-new:a2` (`read` 1/30) — раньше
    /// обеих сессий: `--last 2` их не берёт.
    subagents: bool,
    /// Строки `tokens_unattributed` (сутки UTC от сегодня).
    unattributed: Unattributed,
}

/// Выдача без сессии: сегодня `mcp` 2/30 и `brief` 1/5, три дня назад `mcp` 1/7 (за 30 суток —
/// вызовов 4, токенов 42), 40 дней назад `mcp` 4/100 (вне 30 суток).
const UNATTRIBUTED: Unattributed = &[
    (0, "mcp", 2, 30),
    (0, "brief", 1, 5),
    (3, "mcp", 1, 7),
    (40, "mcp", 4, 100),
];

/// Сутки UTC `days` назад.
fn day_ago(days: i64) -> jiff::civil::Date {
    jiff::Timestamp::now()
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date()
        .checked_sub(jiff::Span::new().days(days))
        .unwrap()
}

/// Сервер в фоновом потоке с учётом двух сессий (`s-old` — holdout, затем `s-new`): адрес и
/// токен `rw` пользователя.
fn start(name: &str) -> (String, String) {
    start_with(name, Seed::default())
}

/// [`start`] с посевом `seed`.
fn start_with(name: &str, seed: Seed) -> (String, String) {
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
            let touch = |id: &str| SessionTouch {
                id: id.into(),
                agent: Some("claude".into()),
                ..SessionTouch::default()
            };
            if seed.subagents {
                for (id, counts) in [
                    (
                        "s-new:a1",
                        vec![count("pre_tool", 2, 20, 0), count("read", 1, 50, 0)],
                    ),
                    ("s-new:a2", vec![count("read", 1, 30, 0)]),
                ] {
                    let sub = SessionTouch {
                        parent_id: Some("s-new".into()),
                        agent_type: Some("Explore".into()),
                        ..touch(id)
                    };
                    zam_store::add_token_counts(&db, &sub, &counts)
                        .await
                        .unwrap();
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            for (ago, event, calls, tokens) in seed.unattributed {
                zam_store::add_unattributed(&db, day_ago(*ago), event, *calls, *tokens)
                    .await
                    .unwrap();
            }
            zam_store::add_token_counts(
                &db,
                &touch("s-old"),
                &[count("session_start", 1, 500, 0), count("read", 2, 80, 0)],
            )
            .await
            .unwrap();
            zam_store::set_holdout(&db, "s-old", true).await.unwrap();
            // последнее обращение позже: `s-new` — свежая
            std::thread::sleep(std::time::Duration::from_millis(20));
            zam_store::add_token_counts(
                &db,
                &touch("s-new"),
                &[
                    count("session_start", 1, 700, 0),
                    count("rules", 1, 300, 0),
                    count("read", 4, 400, 1),
                    count("mcp", 5, 90, 0),
                    count("mcp_read", 3, 0, 2),
                ],
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

fn zam(url: &str, token: &str, cwd: &Path, args: &[&str], session: Option<&str>) -> Output {
    let mut c = common::zam_cmd();
    c.env("ZAM_URL", url)
        .env("ZAM_TOKEN", token)
        .env("HOME", cwd)
        .env("USERPROFILE", cwd)
        .current_dir(cwd)
        .args(args);
    if let Some(s) = session {
        c.env("ZAM_SESSION", s);
    }
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

/// Одна сессия по id и сессия агента (`ZAM_SESSION`): строка сессии, таблица по событиям в
/// порядке событий, итог и доля «уже выдавалось» у `read` и чтений MCP;
/// неизвестная сессия — код 5.
#[test]
fn cli_sessions_tokens_table() {
    let (url, tok) = start("table");
    let r = tmp("table-repo");
    let want = "событие\tвызовов\tтокенов\tуже выдавалось\n\
                session_start\t1\t700\t\n\
                rules\t1\t300\t\n\
                read\t4\t400\t1\n\
                mcp\t5\t90\t\n\
                mcp_read\t3\t0\t2\n\
                итого\t\t1490\t\n\
                уже выдавалось: read 1 из 4 (25 %); чтения MCP 2 из 3 (67 %)\n";
    let out = ok(&zam(&url, &tok, &r, &["sessions", "tokens", "s-new"], None));
    let (head, table) = out.split_once('\n').unwrap();
    assert!(
        head.starts_with("сессия s-new · claude · начало ")
            && head.ends_with(" · эпоха 0")
            && !head.contains('T'),
        "{out}"
    );
    assert_eq!(table, want, "{out}");
    // без id — сессия агента
    let cur = ok(&zam(&url, &tok, &r, &["sessions", "tokens"], Some("s-new")));
    assert_eq!(cur, out);
    let j: Value = serde_json::from_str(&ok(&zam(
        &url,
        &tok,
        &r,
        &["sessions", "tokens", "s-new", "--json"],
        None,
    )))
    .unwrap();
    assert_eq!(
        j["groups"]["all"]["tokens"]["read"]["reread_calls"], 1,
        "{j}"
    );
    assert_eq!(j["items"][0]["id"], "s-new", "{j}");
    let o = zam(&url, &tok, &r, &["sessions", "tokens", "s-none"], None);
    assert_eq!(o.status.code(), Some(5), "{o:?}");
}

/// `--last N` — сумма по N последним сессиям; `--by-holdout` — отдельно основная и контрольная
/// группы; `--last` и id вместе — ошибка ввода.
#[test]
fn cli_sessions_tokens_last() {
    let (url, tok) = start("last");
    let r = tmp("last-repo");
    let out = ok(&zam(
        &url,
        &tok,
        &r,
        &["sessions", "tokens", "--last", "2"],
        None,
    ));
    assert!(out.starts_with("сессий: 2 · с "), "{out}");
    assert!(out.contains("\nsession_start\t2\t1200\t\n"), "{out}");
    assert!(out.contains("\nread\t6\t480\t1\n"), "{out}");
    assert!(out.contains("уже выдавалось: read 1 из 6 (17 %)"), "{out}");
    // одна последняя — свежая
    let one = ok(&zam(
        &url,
        &tok,
        &r,
        &["sessions", "tokens", "--last", "1"],
        None,
    ));
    assert!(one.starts_with("сессия s-new "), "{one}");
    let by = ok(&zam(
        &url,
        &tok,
        &r,
        &["sessions", "tokens", "--last", "2", "--by-holdout"],
        None,
    ));
    let (main, hold) = by
        .split_once("\nконтрольная группа (holdout): сессий 1\n")
        .unwrap_or_else(|| panic!("{by}"));
    assert!(main.contains("основная группа: сессий 1\n"), "{by}");
    assert!(main.contains("\nsession_start\t1\t700\t\n"), "{by}");
    assert!(hold.contains("session_start\t1\t500\t\n"), "{by}");
    assert!(hold.contains("read\t2\t80\t\n"), "{by}");
    let o = zam(
        &url,
        &tok,
        &r,
        &["sessions", "tokens", "s-new", "--last", "2"],
        None,
    );
    assert_eq!(o.status.code(), Some(6), "{o:?}");
}

/// Сервер до M6 (задача 8b) не знает `GET /sessions?tokens=true` и отдаёт сессии без `tokens`:
/// `--last` — ошибка «сервер не поддерживает учёт по сессиям», а не «итого 0» (ревью 8b).
#[test]
fn cli_sessions_tokens_old_server() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        for mut s in listener.incoming().flatten() {
            let mut buf = [0u8; 4096];
            let _ = s.read(&mut buf);
            let body = r#"{"items":[{"id":"s-old","started_at":"2026-11-01T00:00:00Z"}]}"#;
            let _ = write!(
                s,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    let r = tmp("old-repo");
    let o = zam(&url, "t", &r, &["sessions", "tokens", "--last", "3"], None);
    assert_eq!(o.status.code(), Some(1), "{o:?}");
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("сервер не поддерживает учёт по сессиям"),
        "{err}"
    );
}

/// M7 §5.8 п. 2: `--unattributed [--days N]` — выдача без сессии по суткам и событиям
/// (`GET /sessions/unattributed?days=N`): строки «сутки · событие · вызовов · токенов», итог;
/// `--json` — строки сервера и сумма; `--days` вне 1..=366 — ошибка разбора флага, как `--last`
/// вне 1..=500; `--days` без `--unattributed` и `--unattributed` вместе с id, `--last`,
/// `--subagents` — тоже.
#[test]
fn sessions_tokens_unattributed_table() {
    let (url, tok) = start_with(
        "unattributed",
        Seed {
            unattributed: UNATTRIBUTED,
            ..Seed::default()
        },
    );
    let r = tmp("unattributed-repo");
    let (t, d3, d40) = (day_ago(0), day_ago(3), day_ago(40));
    let out = ok(&zam(
        &url,
        &tok,
        &r,
        &["sessions", "tokens", "--unattributed"],
        None,
    ));
    assert_eq!(
        out,
        format!(
            "выдано без сессии за 30 сут. (MCP по HTTP без привязки к сессии агента; сутки UTC)\n\
             сутки\tсобытие\tвызовов\tтокенов\n\
             {t}\tbrief\t1\t5\n\
             {t}\tmcp\t2\t30\n\
             {d3}\tmcp\t1\t7\n\
             итого\t\t4\t42\n"
        )
    );
    let all = ok(&zam(
        &url,
        &tok,
        &r,
        &["sessions", "tokens", "--unattributed", "--days", "366"],
        None,
    ));
    assert!(all.starts_with("выдано без сессии за 366 сут."), "{all}");
    assert!(all.contains(&format!("\n{d40}\tmcp\t4\t100\n")), "{all}");
    assert!(all.ends_with("\nитого\t\t8\t142\n"), "{all}");
    let today = ok(&zam(
        &url,
        &tok,
        &r,
        &["sessions", "tokens", "--unattributed", "--days", "1"],
        None,
    ));
    assert!(
        today.ends_with(&format!("\n{t}\tmcp\t2\t30\nитого\t\t3\t35\n")),
        "{today}"
    );
    let j: Value = serde_json::from_str(&ok(&zam(
        &url,
        &tok,
        &r,
        &[
            "sessions",
            "tokens",
            "--unattributed",
            "--days",
            "7",
            "--json",
        ],
        None,
    )))
    .unwrap();
    assert_eq!(j["days"], 7, "{j}");
    assert_eq!(j["items"].as_array().unwrap().len(), 3, "{j}");
    assert_eq!(
        j["items"][0],
        serde_json::json!({"day": t.to_string(), "event": "brief", "calls": 1, "tokens": 5}),
        "{j}"
    );
    assert_eq!(
        j["total"],
        serde_json::json!({"calls": 4, "tokens": 42}),
        "{j}"
    );
    // `--days` разбирает clap тем же `value_parser!(u32).range(..)`, что `--last`: вне
    // диапазона — тот же код ошибки ввода `zam`, до запроса к серверу
    let last = zam(
        &url,
        &tok,
        &r,
        &["sessions", "tokens", "--last", "501"],
        None,
    );
    assert_eq!(last.status.code(), Some(6), "{last:?}");
    for args in [
        &["sessions", "tokens", "--unattributed", "--days", "400"][..],
        &["sessions", "tokens", "--unattributed", "--days", "0"],
        &["sessions", "tokens", "--days", "7"],
        &["sessions", "tokens", "--unattributed", "s-new"],
        &["sessions", "tokens", "--unattributed", "--last", "2"],
        &["sessions", "tokens", "--unattributed", "--subagents"],
    ] {
        let o = zam(&url, &tok, &r, args, None);
        assert_eq!(o.status.code(), last.status.code(), "{args:?}: {o:?}");
        assert!(o.stdout.is_empty(), "{args:?}: {o:?}");
    }
}

/// M7 §5.8 п. 2: в выводе одной сессии и `--last N` — строка «без сессии за 30 сут.: вызовов C,
/// токенов T — zam sessions tokens --unattributed» при T > 0; T = 0 (выдача без сессии только
/// раньше 30 суток или её нет) — строки нет; в `--json` её нет.
#[test]
fn sessions_tokens_line_when_unattributed() {
    let line =
        "без сессии за 30 сут.: вызовов 4, токенов 42 — zam sessions tokens --unattributed\n";
    let (url, tok) = start_with(
        "line",
        Seed {
            unattributed: UNATTRIBUTED,
            ..Seed::default()
        },
    );
    let r = tmp("line-repo");
    let one = ok(&zam(&url, &tok, &r, &["sessions", "tokens", "s-new"], None));
    assert!(
        one.ends_with(&format!(
            "итого\t\t1490\t\n\
             уже выдавалось: read 1 из 4 (25 %); чтения MCP 2 из 3 (67 %)\n\
             {line}"
        )),
        "{one}"
    );
    let last = ok(&zam(
        &url,
        &tok,
        &r,
        &["sessions", "tokens", "--last", "2", "--by-holdout"],
        None,
    ));
    assert!(last.ends_with(line), "{last}");
    assert_eq!(last.matches("без сессии").count(), 1, "{last}");
    let j = ok(&zam(
        &url,
        &tok,
        &r,
        &["sessions", "tokens", "s-new", "--json"],
        None,
    ));
    assert!(!j.contains("без сессии"), "{j}");
    // выдача без сессии только 40 суток назад — за 30 суток T = 0
    let (url, tok) = start_with(
        "line-old",
        Seed {
            unattributed: &[(40, "mcp", 4, 100)],
            ..Seed::default()
        },
    );
    let out = ok(&zam(&url, &tok, &r, &["sessions", "tokens", "s-new"], None));
    assert!(!out.contains("без сессии"), "{out}");
    // выдачи без сессии нет вовсе
    let (url, tok) = start("line-none");
    let out = ok(&zam(
        &url,
        &tok,
        &r,
        &["sessions", "tokens", "--last", "2"],
        None,
    ));
    assert!(!out.contains("без сессии"), "{out}");
}

/// M7 §5.8 п. 2: `--subagents` — к сессии прибавляются её субагенты (`GET /sessions?parent=`),
/// строка «с субагентами: N»; без флага — только сессия; `--json` — сессия и субагенты в
/// `items`, их число — `subagents`; вместе с `--last` — ошибка ввода.
#[test]
fn sessions_tokens_subagents_sum() {
    let (url, tok) = start_with(
        "subagents",
        Seed {
            subagents: true,
            ..Seed::default()
        },
    );
    let r = tmp("subagents-repo");
    let out = ok(&zam(
        &url,
        &tok,
        &r,
        &["sessions", "tokens", "s-new", "--subagents"],
        None,
    ));
    let mut lines = out.splitn(3, '\n');
    let head = lines.next().unwrap();
    assert!(head.starts_with("сессия s-new · claude · "), "{out}");
    assert_eq!(lines.next(), Some("с субагентами: 2"), "{out}");
    assert_eq!(
        lines.next().unwrap(),
        "событие\tвызовов\tтокенов\tуже выдавалось\n\
         session_start\t1\t700\t\n\
         rules\t1\t300\t\n\
         pre_tool\t2\t20\t\n\
         read\t6\t480\t1\n\
         mcp\t5\t90\t\n\
         mcp_read\t3\t0\t2\n\
         итого\t\t1590\t\n\
         уже выдавалось: read 1 из 6 (17 %); чтения MCP 2 из 3 (67 %)\n",
        "{out}"
    );
    // сессия агента (`ZAM_SESSION`) — то же
    let cur = ok(&zam(
        &url,
        &tok,
        &r,
        &["sessions", "tokens", "--subagents"],
        Some("s-new"),
    ));
    assert_eq!(cur, out);
    // без флага — только сессия
    let plain = ok(&zam(&url, &tok, &r, &["sessions", "tokens", "s-new"], None));
    assert!(plain.contains("\nитого\t\t1490\t\n"), "{plain}");
    assert!(!plain.contains("субагент"), "{plain}");
    let j: Value = serde_json::from_str(&ok(&zam(
        &url,
        &tok,
        &r,
        &["sessions", "tokens", "s-new", "--subagents", "--json"],
        None,
    )))
    .unwrap();
    assert_eq!(j["subagents"], 2, "{j}");
    assert_eq!(j["items"][0]["id"], "s-new", "{j}");
    assert_eq!(j["items"].as_array().unwrap().len(), 3, "{j}");
    assert_eq!(j["groups"]["all"]["sessions"], 3, "{j}");
    assert_eq!(j["groups"]["all"]["tokens"]["read"]["calls"], 6, "{j}");
    // `--last 2` субагентов не берёт (они раньше обеих сессий); `--subagents` с ним — ошибка
    let last = ok(&zam(
        &url,
        &tok,
        &r,
        &["sessions", "tokens", "--last", "2"],
        None,
    ));
    assert!(last.contains("\nread\t6\t480\t1\n"), "{last}");
    let o = zam(
        &url,
        &tok,
        &r,
        &["sessions", "tokens", "--last", "2", "--subagents"],
        None,
    );
    assert_eq!(o.status.code(), Some(6), "{o:?}");
}

/// Review Focus 5 (задача 9 M7.2): сервер до M6 на `GET /sessions/unattributed` отвечает 404
/// (это путь `GET /sessions/{id}`), признаков в `/health` нет. Вывод сессии — без строки «без
/// сессии», код 0; `--unattributed` и `--subagents` (400 на неизвестный параметр `parent`) —
/// код 1 с подсказкой обновить сервер.
#[test]
fn sessions_tokens_old_server_no_line() {
    let (url, seen) = common::stub_with(std::sync::Arc::new(|_: &str, p: &str, _: usize| {
        let now = std::time::Duration::ZERO;
        if p == "/api/v1/health" {
            return (200, r#"{"status":"ok","version":"0.0.10-m5.7"}"#, now);
        }
        if p.starts_with("/api/v1/sessions?") {
            let bad = r#"{"type":"/problems/bad-request","status":400,"detail":"unknown field `parent`"}"#;
            return (400, bad, now);
        }
        if p == "/api/v1/sessions/s-old" {
            let s = r#"{"id":"s-old","agent":"claude","started_at":"2026-11-01T00:00:00Z",
                        "last_seen_at":"2026-11-01T00:00:00Z",
                        "tokens":{"read":{"calls":1,"tokens":10}}}"#;
            return (200, s, now);
        }
        (404, r#"{"type":"/problems/not-found","status":404}"#, now)
    }));
    let r = tmp("old-line-repo");
    let out = ok(&zam(&url, "t", &r, &["sessions", "tokens", "s-old"], None));
    assert!(
        out.ends_with("\nитого\t\t10\t\nуже выдавалось: read 0 из 1 (0 %)\n"),
        "{out}"
    );
    assert!(!out.contains("без сессии"), "{out}");
    let asked: Vec<String> = seen
        .lock()
        .unwrap()
        .iter()
        .map(|(_, p, _)| p.clone())
        .collect();
    assert!(
        asked
            .iter()
            .any(|p| p.starts_with("/api/v1/sessions/unattributed")),
        "{asked:?}"
    );
    for args in [
        &["sessions", "tokens", "--unattributed"][..],
        &["sessions", "tokens", "s-old", "--subagents"],
    ] {
        let o = zam(&url, "t", &r, args, None);
        assert_eq!(o.status.code(), Some(1), "{args:?}: {o:?}");
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(err.contains("обновите zam-server"), "{args:?}: {err}");
        assert!(o.stdout.is_empty(), "{args:?}: {o:?}");
    }
}
