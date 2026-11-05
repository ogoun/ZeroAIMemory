//! Привязка `zam mcp` к сессии агента (M6 §7.1, задача 3): хук `session-start` пишет
//! `.zeroaimemory/cache/agent-<pid>.json` на предков до процесса агента, `zam mcp` ищет файл
//! по своей цепочке предков и шлёт `X-Zam-Session`. Сильнее файла — `ZAM_SESSION_ID`.
//! `stdio_mcp_session_accounting` — тот же путь против настоящего сервера: отметки выданного,
//! пометка «(выдавалось)» и учёт `mcp` в сессии агента.
//!
//! У процесса `zam` в тесте агента среди предков нет (родитель — сам процесс теста), поэтому
//! файл пишется на непосредственного родителя — pid теста; `zam mcp`, запущенный тем же
//! тестом, находит его по тому же pid. Подмена цепочки предков (Codex: хук и MCP через разные
//! оболочки) — юнит-тест `binding_found_via_fake_ancestors` в `src/cmd/hook/binding.rs`.
//! Сервер MCP — заглушка, записывающая заголовки запросов.
mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

/// Тесты файла — по одному: у всех один родитель (pid теста), и файлы привязки разных тестов
/// лежат в разных каталогах, но заглушки и процессы лучше не перемешивать.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Проект теста: `w/.zeroaimemory/` (корень проекта) и «домашний» каталог рядом.
struct Proj {
    repo: PathBuf,
    home: PathBuf,
}

impl Proj {
    fn new(name: &str) -> Proj {
        let repo = common::sandbox("cli-mcp-bind", name);
        std::fs::create_dir_all(repo.join(".zeroaimemory")).unwrap();
        let home = repo.parent().unwrap().join("home");
        std::fs::create_dir_all(home.join(".zeroaimemory")).unwrap();
        // хук не запускает фоновую `zam code sync`
        std::fs::write(
            home.join(".zeroaimemory/config.toml"),
            "[code]\nauto = false\n",
        )
        .unwrap();
        Proj { repo, home }
    }

    fn cmd(&self) -> assert_cmd::Command {
        let mut c = common::zam_cmd();
        c.env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .current_dir(&self.repo);
        c
    }

    /// `zam hook session-start` без сервера (привязка пишется и без него).
    fn session_start(&self, session: &str, source: &str) {
        let ev = json!({
            "session_id": session,
            "transcript_path": self.repo.join("t.jsonl"),
            "cwd": self.repo,
            "hook_event_name": "SessionStart",
            "source": source,
        });
        let o = self
            .cmd()
            .args(["hook", "session-start"])
            .write_stdin(ev.to_string())
            .output()
            .unwrap();
        assert!(o.status.success(), "{o:?}");
    }

    /// Файл привязки процесса `pid`.
    fn binding_file(&self, pid: u32) -> PathBuf {
        binding_path(&self.repo, pid)
    }
}

fn binding_path(repo: &Path, pid: u32) -> PathBuf {
    repo.join(format!(".zeroaimemory/cache/agent-{pid}.json"))
}

fn read_binding_file(p: &Path) -> Value {
    let t = std::fs::read_to_string(p)
        .unwrap_or_else(|e| panic!("нет файла привязки {}: {e}", p.display()));
    serde_json::from_str(&t).unwrap()
}

/// `session-start` пишет привязку на процесс агента; `/clear` и `resume` (новая сессия у того
/// же процесса) её переписывают.
#[test]
fn binding_written_on_session_start_clear_resume() {
    let _g = serial();
    let p = Proj::new("start-clear-resume");
    let me = std::process::id();
    let f = p.binding_file(me);
    p.session_start("sess-a", "startup");
    let v = read_binding_file(&f);
    assert_eq!(v["session_id"], "sess-a", "{v}");
    let started = v["started_at"].as_str().unwrap().to_string();
    assert!(
        started.len() == 20 && started.ends_with('Z'),
        "RFC 3339 UTC: {started}"
    );
    p.session_start("sess-b", "clear");
    assert_eq!(read_binding_file(&f)["session_id"], "sess-b");
    p.session_start("sess-c", "resume");
    assert_eq!(read_binding_file(&f)["session_id"], "sess-c");
    // `compact` — та же сессия
    p.session_start("sess-c", "compact");
    assert_eq!(read_binding_file(&f)["session_id"], "sess-c");
}

/// Заголовки запросов к заглушке `/mcp`.
type Heads = Arc<Mutex<Vec<Vec<(String, String)>>>>;

/// Заглушка MCP: на каждый POST — ответ JSON-RPC с тем же `id`; заголовки — в [`Heads`].
fn mcp_stub() -> (String, Heads) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let heads: Heads = Default::default();
    let h2 = heads.clone();
    std::thread::spawn(move || {
        for conn in l.incoming().flatten() {
            let h3 = h2.clone();
            std::thread::spawn(move || {
                let mut r = BufReader::new(conn.try_clone().unwrap());
                let mut line = String::new();
                if r.read_line(&mut line).is_err() {
                    return;
                }
                let mut hs = Vec::new();
                let mut len = 0usize;
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).is_err() || h.trim().is_empty() {
                        break;
                    }
                    if let Some((k, v)) = h.split_once(':') {
                        let (k, v) = (k.trim().to_ascii_lowercase(), v.trim().to_string());
                        if k == "content-length" {
                            len = v.parse().unwrap_or(0);
                        }
                        hs.push((k, v));
                    }
                }
                let mut b = vec![0u8; len];
                let _ = r.read_exact(&mut b);
                let req: Value = serde_json::from_slice(&b).unwrap_or(Value::Null);
                h3.lock().unwrap().push(hs);
                let body = json!({"jsonrpc": "2.0", "id": req["id"],
                                  "result": {"protocolVersion": "2025-06-18"}})
                .to_string();
                let mut c = conn;
                let _ = write!(
                    c,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            });
        }
    });
    (url, heads)
}

/// `zam mcp` с `initialize` и одним `tools/call`; значения `X-Zam-Session` запросов (`None` —
/// без заголовка).
fn mcp_sessions(p: &Proj, env: &[(&str, &str)]) -> Vec<Option<String>> {
    let (url, heads) = mcp_stub();
    let stdin = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                          "clientInfo": {"name": "t", "version": "1"}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": "memory_search", "arguments": {"query": "x"}}}),
    ]
    .map(|v| v.to_string())
    .join("\n")
        + "\n";
    let mut c = p.cmd();
    c.env("ZAM_URL", &url).env("ZAM_TOKEN", "zam_test_token");
    for (k, v) in env {
        c.env(k, v);
    }
    let o = c.arg("mcp").write_stdin(stdin).output().unwrap();
    assert!(o.status.success(), "{o:?}");
    let out = String::from_utf8_lossy(&o.stdout);
    assert_eq!(out.lines().count(), 2, "{out}");
    let h = heads.lock().unwrap();
    assert_eq!(h.len(), 2, "запросов к /mcp");
    h.iter()
        .map(|hs| {
            hs.iter()
                .find(|(k, _)| k == "x-zam-session")
                .map(|(_, v)| v.clone())
        })
        .collect()
}

/// Без переменных — сессия из файла привязки своего процесса агента; без файла — без
/// заголовка (неявный путь сервера).
#[test]
fn mcp_uses_binding_without_env() {
    let _g = serial();
    let p = Proj::new("binding-used");
    assert_eq!(mcp_sessions(&p, &[]), [None, None]);
    p.session_start("sess-bound", "startup");
    let s = Some("sess-bound".to_string());
    assert_eq!(mcp_sessions(&p, &[]), [s.clone(), s]);
}

/// `ZAM_SESSION_ID` сильнее файла привязки; `CLAUDE_CODE_SESSION_ID` не читается (у Claude Code
/// такой переменной для локальной сессии нет — ревью задачи 3, I4).
#[test]
fn env_session_beats_binding() {
    let _g = serial();
    let p = Proj::new("env-beats");
    p.session_start("sess-bound", "startup");
    let one = |s: &str| [Some(s.to_string()), Some(s.to_string())];
    assert_eq!(
        mcp_sessions(&p, &[("ZAM_SESSION_ID", "sess-env")]),
        one("sess-env")
    );
    assert_eq!(
        mcp_sessions(&p, &[("CLAUDE_CODE_SESSION_ID", "sess-cc")]),
        one("sess-bound")
    );
    // некорректное значение переменной не используется — дальше по порядку
    assert_eq!(
        mcp_sessions(&p, &[("ZAM_SESSION_ID", "  ")]),
        one("sess-bound")
    );
}

/// Сервер в фоновом потоке (без моделей, FTS): адрес и токен `rw` пользователя «Анна».
fn server(name: &str) -> (String, String) {
    use zam_server::build;
    use zam_server::config::ServerConfig;
    let data = common::sandbox("cli-mcp-bind-data", name);
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
            cfg.recall.timeout_ms = 30_000;
            let (app, st, _) = build(cfg, Some("pw".into())).await.unwrap();
            let u = st.system.create_user("Анна", false).await.unwrap();
            let (_, t) = st
                .system
                .issue_token(u.id, "t", zam_store::Access::Rw)
                .await
                .unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            tx.send((url, t)).unwrap();
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });
    });
    rx.recv().unwrap()
}

/// Тело ответа как JSON.
fn body(r: ureq::http::Response<ureq::Body>) -> Value {
    let t = r.into_body().read_to_string().unwrap();
    serde_json::from_str(&t).unwrap_or(Value::Null)
}

fn http() -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .proxy(None)
            .http_status_as_error(false)
            .build(),
    )
}

/// `zam mcp` против настоящего сервера: `initialize` и один `tools/call`; текст результата.
fn mcp_call(p: &Proj, url: &str, tok: &str, name: &str, args: Value) -> String {
    let stdin = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                          "clientInfo": {"name": "t", "version": "1"}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": name, "arguments": args}}),
    ]
    .map(|v| v.to_string())
    .join("\n")
        + "\n";
    let o = p
        .cmd()
        .env("ZAM_URL", url)
        .env("ZAM_TOKEN", tok)
        .arg("mcp")
        .write_stdin(stdin)
        .output()
        .unwrap();
    assert!(o.status.success(), "{o:?}");
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    let call = out
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .find(|v| v["id"] == 2)
        .unwrap_or_else(|| panic!("нет ответа на tools/call: {out}"));
    assert_ne!(call["result"]["isError"], true, "{call}");
    call["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_string()
}

/// stdio-путь целиком (ревью задачи 3, I4): `session-start` пишет привязку, `zam mcp` шлёт
/// сессию — `/read` отмечает выданное, `memory_search` даёт «(выдавалось)» с фрагментом, учёт
/// — событие `mcp` сессии агента, а не `unattributed`.
#[test]
fn stdio_mcp_session_accounting() {
    let _g = serial();
    let (url, tok) = server("stdio");
    let p = Proj::new("stdio");
    let a = http();
    let auth = format!("Bearer {tok}");
    let r = a
        .post(&format!("{url}/api/v1/nodes"))
        .header("Authorization", &auth)
        .content_type("application/json")
        .send(
            json!({"space": "knowledge", "path": "Астро/Квазар", "kind": "note",
                   "content": "# Квазар\n\nКвазар светит ярко.\n"})
            .to_string(),
        )
        .unwrap();
    assert_eq!(r.status(), 201);
    let id = body(r)["id"].as_str().unwrap().to_string();
    p.session_start("sess-stdio", "startup");
    let read = mcp_call(
        &p,
        &url,
        &tok,
        "memory_read",
        json!({"items": [{"id": format!("n:{id}")}]}),
    );
    assert!(read.contains("светит ярко"), "{read}");
    let found = mcp_call(&p, &url, &tok, "memory_search", json!({"query": "квазар"}));
    assert!(
        found.contains("(выдавалось)") && found.contains("светит"),
        "{found}"
    );
    // учёт пишется в фоне — опрос до 10 с
    let t = std::time::Instant::now();
    let tokens = loop {
        let v: Value = a
            .get(&format!("{url}/api/v1/sessions/sess-stdio"))
            .header("Authorization", &auth)
            .call()
            .map(body)
            .unwrap();
        if v["tokens"]["mcp"]["calls"] == 2 || t.elapsed() > std::time::Duration::from_secs(10) {
            break v["tokens"].clone();
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    assert_eq!(tokens["mcp"]["calls"], 2, "{tokens}");
    let u: Value = a
        .get(&format!("{url}/api/v1/sessions/unattributed?days=1"))
        .header("Authorization", &auth)
        .call()
        .map(body)
        .unwrap();
    assert_eq!(u["items"], json!([]), "{u}");
}
