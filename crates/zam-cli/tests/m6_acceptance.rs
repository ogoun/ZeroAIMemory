//! Приёмка M6 (спецификация M6 §17, задача 13b), клиентская часть — только новые по сути
//! проверки, с **настоящими** бинарниками тега `v0.0.10-m5.7` (`target/tags/v0.0.10-m5.7/release`,
//! сборка задачи 1a2; другие пути — `ZAM_M57_SERVER_BIN`, `ZAM_M57_ZAM_BIN`). Нет бинарника —
//! пропуск с сообщением (в CI их нет).
//!
//! - `compat_new_zam_old_server` (§17 п. 13, 17) — `zam` M6 против сервера v0.0.10-m5.7: поиск и
//!   чтение; `--focus` — понятная ошибка «unknown field `focus`»; хуки (`session-start`,
//!   `prompt`, `post-tool`, `PreCompact` с `new_epoch` → 400 молча, `SessionEnd` со снимком —
//!   снимок в очереди `snapq-*`, хук без ошибки), `hooks.holdout` → 400 молча; идеи; `zam mcp`;
//!   `zam code sync` без `recv`;
//! - `compat_old_zam_new_server` (§17 п. 13, 17) — `zam` v0.0.10-m5.7 против сервера M6: поиск и
//!   чтение; хуки; новые события (`SessionEnd`, `PostCompact` на `zam hook checkpoint`) — код 0 и
//!   пустой вывод; эпоха по
//!   `source=compact` при `[hooks] recovery = true`; идеи; `zam mcp` (неявный путь);
//!   `zam code sync` (`recv_count` 0);
//! - `acceptance_hook_budgets` (§17 п. 15, `#[ignore]`, тихое окно) — p95 `/recall(prompt)`
//!   release-сервера ветки и **предыдущего выпуска** ([`PREV_RELEASE`], `target/tags/<тег>/release`
//!   или `ZAM_PREV_SERVER_BIN`) вперемешку: медиана по раундам отношения p95 ≤ 1,10 (M7.1 §4.8
//!   п. 3 **[решение координатора]**: база — предыдущий выпуск, а не замороженная M5.7; накопленный
//!   рост держат абсолютные пороги `m57_acceptance` и бюджеты хуков).
//!   Прочие бюджеты хуков §17 п. 15 — release-серии `m4_acceptance::hook_budgets*` задачи 17d
//!   (`relr2-1`, `relr2-2` — 4 из 4, журнал этапа), `m56_acceptance::acceptance_hook_budgets_with_map`,
//!   `m57_acceptance::acceptance_recall_p95_with_50_ideas`.
//!
//! Снимок сессии (§17 п. 19) — `hooks.rs`, `snapshot.rs`, `m4_acceptance::hook_budgets_snapshot_*`
//! на живых фикстурах Claude (`tests/fixtures/transcripts/claude-2.1.314-*`); живого журнала Codex
//! нет (задача 17a, шаг 0) — п. 19 для Codex открыт. Остальные пункты §17 — таблица «пункт →
//! тесты» в журнале этапа («Задача 13b») и `crates/zam-server/tests/m6_acceptance.rs`.
mod common;

use std::io::{BufRead as _, BufReader};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use zam_server::build;
use zam_server::config::ServerConfig;
use zam_store::Access;

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e61";
/// `sessionId` записей журналов-фикстур Claude Code (`tests/fixtures/transcripts`): снимок
/// берёт только записи своей сессии.
const FIXTURE_SESSION: &str = "00000000-0000-4000-8000-000000000001";

fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// Бинарник тега v0.0.10-m5.7: переменная `var` или `target/tags/v0.0.10-m5.7/release/<name>`.
fn tag_bin(var: &str, name: &str) -> Option<PathBuf> {
    let p = std::env::var_os(var).map(PathBuf::from).unwrap_or_else(|| {
        common::repo_root()
            .join("target/tags/v0.0.10-m5.7/release")
            .join(exe(name))
    });
    if p.is_file() {
        Some(p)
    } else {
        eprintln!("нет {} ({var}) — пропуск", p.display());
        None
    }
}

fn http() -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .proxy(None)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(120)))
            .build(),
    )
}

fn rest(url: &str, tok: &str, method: &str, path: &str, body: Option<&Value>) -> (u16, Value) {
    rest_h(url, tok, method, path, body, &[])
}

fn rest_h(
    url: &str,
    tok: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
    headers: &[(&str, &str)],
) -> (u16, Value) {
    let a = http();
    let uri = format!("{url}/api/v1{path}");
    let auth = format!("Bearer {tok}");
    let r = match method {
        "GET" => {
            let mut q = a.get(&uri).header("Authorization", &auth);
            for (k, v) in headers {
                q = q.header(*k, *v);
            }
            q.call()
        }
        _ => {
            let mut q = a.post(&uri).header("Authorization", &auth);
            for (k, v) in headers {
                q = q.header(*k, *v);
            }
            q.content_type("application/json")
                .send(body.map(Value::to_string).unwrap_or_default())
        }
    };
    let mut r = r.unwrap_or_else(|e| panic!("{method} {path}: {e}"));
    let code = r.status().as_u16();
    let text = r.body_mut().read_to_string().unwrap_or_default();
    let v = if text.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or(Value::String(text))
    };
    (code, v)
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// `zam-server serve` другим исполняемым файлом (бинарник тега или release M6) без моделей.
struct Srv {
    child: Child,
    url: String,
    token: String,
}

impl Srv {
    fn start(exe: &Path, name: &str) -> Srv {
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("cli-m6-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let port = free_port();
        let conf = root.join("zam.toml");
        std::fs::write(
            &conf,
            format!(
                "data_dir = '{}'\n\n[server]\nbind = \"127.0.0.1:{port}\"\n\n[recall]\n\
                 timeout_ms = 2000\n",
                root.join("data").display()
            ),
        )
        .unwrap();
        let mut cmd = std::process::Command::new(exe);
        for (k, _) in std::env::vars().filter(|(k, _)| k.starts_with("ZAM_")) {
            cmd.env_remove(k);
        }
        let mut child = cmd
            .args(["serve", "--config"])
            .arg(&conf)
            .env("ZAM_ADMIN_PASSWORD", "pw")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("{}: {e}", exe.display()));
        let (tx, rx) = std::sync::mpsc::channel();
        let stderr = child.stderr.take().unwrap();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if let Some(p) = line.strip_prefix(zam_server::INITIAL_TOKEN_FILE_LINE)
                    && let Ok(t) = std::fs::read_to_string(p.trim())
                {
                    let _ = tx.send(t.trim().to_string());
                }
            }
        });
        let token = rx
            .recv_timeout(Duration::from_secs(120))
            .expect("первый токен сервера");
        let url = format!("http://127.0.0.1:{port}");
        let t0 = Instant::now();
        while rest(&url, &token, "GET", "/health", None).0 != 200 {
            assert!(t0.elapsed() < Duration::from_secs(60), "сервер не ответил");
            std::thread::sleep(Duration::from_millis(100));
        }
        let (c, v) = rest(
            &url,
            &token,
            "POST",
            "/solutions",
            Some(&json!({"agent_solution_id": G, "name": "Совместимость M6"})),
        );
        assert!((200..300).contains(&c), "{v}");
        Srv { child, url, token }
    }

    fn version(&self) -> String {
        rest(&self.url, &self.token, "GET", "/health", None).1["version"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }
}

impl Drop for Srv {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Сервер M6 в процессе теста (без моделей) с решением `G`: адрес и токен.
fn m6_server(name: &str) -> (String, String) {
    let data = common::sandbox("cli-m6", &format!("{name}-data"));
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
    let (url, tok): (String, String) = rx.recv().unwrap();
    let (c, v) = rest(
        &url,
        &tok,
        "POST",
        "/solutions",
        Some(&json!({"agent_solution_id": G, "name": "Совместимость M6"})),
    );
    assert_eq!(c, 201, "{v}");
    (url, tok)
}

/// Проект решения `G` с клиентом `zam` (M6 — `None`, тег — путь).
struct Proj {
    url: String,
    token: String,
    repo: PathBuf,
    home: PathBuf,
    zam: Option<PathBuf>,
}

impl Proj {
    fn new(name: &str, url: &str, token: &str, zam: Option<PathBuf>, hooks: &str) -> Proj {
        let repo = common::sandbox("cli-m6", name);
        let home = repo.parent().unwrap().join(format!("{name}-home"));
        std::fs::create_dir_all(home.join(".zeroaimemory")).unwrap();
        std::fs::write(
            home.join(".zeroaimemory/config.toml"),
            format!("[code]\nauto = false\n\n[hooks]\n{hooks}"),
        )
        .unwrap();
        std::fs::create_dir_all(repo.join(".zeroaimemory")).unwrap();
        std::fs::write(
            repo.join(".zeroaimemory/solution.toml"),
            format!("agent_solution_id = \"{G}\"\nname = \"Совместимость\"\n"),
        )
        .unwrap();
        Proj {
            url: url.into(),
            token: token.into(),
            repo,
            home,
            zam,
        }
    }

    /// `zam` с чистым окружением (`common::zam_cmd` — и для бинарника тега).
    fn base(&self) -> assert_cmd::Command {
        let mut c = match &self.zam {
            None => common::zam_cmd(),
            Some(p) => {
                let mut c = assert_cmd::Command::new(p);
                for k in common::PROXY_VARS {
                    c.env_remove(k);
                }
                for k in zam_core::AGENT_ENV_VARS
                    .iter()
                    .chain(&zam_core::CLIENT_ENV_VARS)
                {
                    c.env_remove(k);
                }
                for (k, _) in std::env::vars().filter(|(k, _)| k.starts_with("ZAM_")) {
                    c.env_remove(k);
                }
                c
            }
        };
        c.env("ZAM_URL", &self.url)
            .env("ZAM_TOKEN", &self.token)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .current_dir(&self.repo);
        c
    }

    fn z(&self, args: &[&str], stdin: &[u8]) -> Output {
        self.base()
            .args(args)
            .write_stdin(stdin.to_vec())
            .output()
            .unwrap()
    }

    /// `zam hook <kind>`: у отладочного `zam` M6 — щедрый бюджет (содержимое, а не скорость:
    /// время — release-серии `hook_budgets`), у release тега — бюджет продукта.
    fn hook(&self, kind: &str, ev: &Value) -> Output {
        let mut c = self.base();
        if self.zam.is_none() {
            c.env("ZAM_TEST_HOOK_BUDGET_MS", "10000");
        }
        c.args(["hook", kind])
            .write_stdin(ev.to_string().into_bytes())
            .output()
            .unwrap()
    }

    fn ev(&self, event: &str, session: &str, extra: Value) -> Value {
        let mut v = json!({"session_id": session, "transcript_path": self.repo.join("t.jsonl"),
                           "cwd": self.repo, "hook_event_name": event});
        for (k, x) in extra.as_object().unwrap() {
            v[k] = x.clone();
        }
        v
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.repo.join(".zeroaimemory/logs/hooks.log")).unwrap_or_default()
    }

    fn cache_files(&self, prefix: &str) -> Vec<String> {
        std::fs::read_dir(self.repo.join(".zeroaimemory/cache"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(prefix))
            .collect()
    }

    /// `zam mcp` (stdio): `initialize`, `tools/list`, `memory_search`; ответы по строкам.
    fn mcp(&self, env: &[(&str, &str)]) -> Vec<Value> {
        let stdin = [
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                   "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                              "clientInfo": {"name": "t", "version": "1"}}}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                   "params": {"name": "memory_search", "arguments": {"query": "кэш"}}}),
        ]
        .map(|v| v.to_string())
        .join("\n")
            + "\n";
        let mut c = self.base();
        for (k, v) in env {
            c.env(k, v);
        }
        let o = c.arg("mcp").write_stdin(stdin).output().unwrap();
        assert!(o.status.success(), "zam mcp: {}", err(&o));
        out(&o)
            .lines()
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
            .collect()
    }

    /// Проект из трёх файлов для `zam code sync`.
    fn code_files(&self) {
        let src = self.repo.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("lib.rs"),
            "pub struct Cache;\nimpl Cache {\n    pub fn get(&self) -> u32 { helper() }\n}\n\
             fn helper() -> u32 { 1 }\n",
        )
        .unwrap();
        std::fs::write(
            src.join("app.ts"),
            "export function start(): number {\n  return 1;\n}\n",
        )
        .unwrap();
        std::fs::write(src.join("tool.py"), "def run():\n    return 1\n").unwrap();
    }
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn ok(o: &Output, what: &str) -> String {
    assert_eq!(
        o.status.code(),
        Some(0),
        "{what}\nstdout: {}\nstderr: {}",
        out(o),
        err(o)
    );
    out(o)
}

/// `additionalContext` вывода хука (пусто — пустая строка).
fn context(o: &Output) -> String {
    let s = ok(o, "hook");
    if s.trim().is_empty() {
        return String::new();
    }
    let v: Value = serde_json::from_str(s.trim()).unwrap_or_else(|e| panic!("{e}: {s}"));
    v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

const NOTE: &str = "# Кэш миниатюр\n\nКэш миниатюр чистится раз в сутки.\n";

/// Общие шаги обоих направлений: запись, поиск, чтение, идея в дайджесте (раздел `ideas` —
/// по серверу: M5.7 — `### Идеи`, текущий в режиме по умолчанию `review` — `### Идеи на
/// разбор`), MCP, `code sync`.
fn common_steps(p: &Proj, who: &str, ideas: &str) {
    ok(
        &p.z(&["write", "knowledge:m6/cache"], NOTE.as_bytes()),
        &format!("{who}: write"),
    );
    let s = ok(
        &p.z(&["search", "кэш миниатюр"], b""),
        &format!("{who}: search"),
    );
    assert!(s.contains("m6/cache"), "{who}: search: {s}");
    let r = p.z(&["read", "knowledge:m6/cache"], b"");
    assert_eq!(ok(&r, &format!("{who}: read")), NOTE, "{who}: read");
    let i = ok(
        &p.z(
            &["idea", "new", "Экспорт отчётов в PDF", "--origin", "human"],
            b"",
        ),
        &format!("{who}: idea new"),
    );
    let idea = i
        .trim()
        .split_once('@')
        .map_or(i.trim(), |(id, _)| id)
        .to_string();
    let ctx = context(&p.hook(
        "session-start",
        &p.ev(
            "SessionStart",
            &format!("{who}-ideas"),
            json!({"source": "startup"}),
        ),
    ));
    assert!(
        ctx.contains(&format!("{ideas}\n")) && ctx.contains(&format!("w:{idea}")),
        "{who}: идея в дайджесте: {ctx}\n{}",
        p.log()
    );
    // ответы — по `id`: после `initialize` `zam mcp` обрабатывает сообщения параллельно и пишет
    // ответы в порядке готовности (`memory_search` бывает раньше `tools/list`; M6.6, задача 12)
    let m = p.mcp(&[]);
    assert_eq!(m.len(), 3, "{who}: {m:?}");
    let by_id = |id: u64| {
        m.iter()
            .find(|r| r["id"] == id)
            .unwrap_or_else(|| panic!("{who}: нет ответа id {id}: {m:?}"))
    };
    let tools = by_id(2)["result"]["tools"].as_array().map_or(0, Vec::len);
    assert!(tools >= 11, "{who}: tools/list — {tools} инструментов");
    let search = by_id(3);
    let found = search["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(found.contains("m6/cache"), "{who}: memory_search: {search}");
    p.code_files();
    let o = p.z(&["code", "sync", "--full"], b"");
    ok(&o, &format!("{who}: code sync"));
}

/// §17 п. 13, 17: `zam` M6 против сервера v0.0.10-m5.7.
#[test]
fn compat_new_zam_old_server() {
    let _g = serial();
    let Some(srv) = tag_bin("ZAM_M57_SERVER_BIN", "zam-server") else {
        return;
    };
    let s = Srv::start(&srv, "new-zam-old-srv");
    assert_eq!(s.version(), "0.0.10-m5.7");
    let p = Proj::new("nz-os", &s.url, &s.token, None, "recovery = true\n");
    common_steps(&p, "zam M6 → сервер M5.7", "### Идеи");
    // фокус: старый ReadItem — 400 «unknown field `focus`», код выхода не 0, причина названа
    let o = p.z(&["read", "knowledge:m6/cache", "--focus", "кэш"], b"");
    assert_ne!(
        o.status.code(),
        Some(0),
        "focus у старого сервера: {}",
        out(&o)
    );
    assert!(err(&o).contains("focus"), "причина: {}", err(&o));
    // хуки сессии: старт, запрос, ошибка инструмента, PreCompact (new_epoch → 400 молча)
    let sid = "nz-os-1";
    let ctx = context(&p.hook(
        "session-start",
        &p.ev("SessionStart", sid, json!({"source": "startup"})),
    ));
    assert!(ctx.contains("ZeroAIMemory"), "дайджест: {ctx}");
    ok(
        &p.hook(
            "prompt",
            &p.ev(
                "UserPromptSubmit",
                sid,
                json!({"prompt": "почисти кэш миниатюр"}),
            ),
        ),
        "prompt",
    );
    ok(
        &p.hook(
            "post-tool",
            &p.ev(
                "PostToolUse",
                sid,
                json!({"tool_name": "Bash", "tool_input": {"command": "cargo build"},
                       "tool_response": {"stderr": "error[E0382]: borrow of moved value",
                                         "exit_code": 101}}),
            ),
        ),
        "post-tool",
    );
    let before = p.log();
    let o = p.hook(
        "checkpoint",
        &p.ev("PreCompact", sid, json!({"trigger": "auto"})),
    );
    ok(&o, "PreCompact");
    let added = p.log()[before.len()..].to_string();
    assert!(
        !added.contains("new_epoch") || !added.contains("HTTP 400"),
        "400 на new_epoch у старого сервера — молча: {added}"
    );
    // SessionEnd со снимком: у сервера M5.7 маршрута нет — снимок в очереди, хук без ошибки
    let transcript = p.repo.join("transcript.jsonl");
    let fixture = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/transcripts/claude-2.1.314-tools.jsonl"),
    )
    .unwrap();
    let root = serde_json::to_string(&p.repo.to_string_lossy()).unwrap();
    std::fs::write(
        &transcript,
        fixture.replace("@ROOT@", &root[1..root.len() - 1]),
    )
    .unwrap();
    for i in 0..4 {
        let ev = json!({"session_id": FIXTURE_SESSION, "transcript_path": transcript,
                        "cwd": p.repo, "hook_event_name": "SessionEnd", "reason": "other"});
        // `zam init` M6 вешает SessionEnd и PostCompact на `zam hook checkpoint`
        let o = p.hook("checkpoint", &ev);
        assert_eq!(out(&o).trim(), "", "SessionEnd {i}: вывод агенту пуст");
        ok(&o, "SessionEnd");
    }
    let log = p.log();
    assert!(
        log.contains("старый сервер") && !p.cache_files("snapq-").is_empty(),
        "снимок со старым сервером — в очереди: {:?}\n{log}",
        p.cache_files("snap")
    );
    // контрольная группа A/B: POST /sessions {holdout} у старого сервера — 400 молча
    let q = Proj::new("nz-os-holdout", &s.url, &s.token, None, "holdout = 0.5\n");
    let hs = common::holdout_session("nz-os-h", 0.5, true);
    let o = q.hook(
        "session-start",
        &q.ev("SessionStart", &hs, json!({"source": "startup"})),
    );
    ok(&o, "session-start holdout");
    assert!(
        !q.log().contains("HTTP 400"),
        "400 holdout — молча: {}",
        q.log()
    );
}

/// §17 п. 13, 17: `zam` v0.0.10-m5.7 против сервера M6.
#[test]
fn compat_old_zam_new_server() {
    let _g = serial();
    let Some(zam) = tag_bin("ZAM_M57_ZAM_BIN", "zam") else {
        return;
    };
    let (url, tok) = m6_server("old-zam-new-srv");
    let p = Proj::new("oz-ns", &url, &tok, Some(zam), "recovery = true\n");
    let v = ok(&p.z(&["--version"], b""), "zam --version");
    assert!(v.contains("0.0.10-m5.7"), "{v}");
    common_steps(&p, "zam M5.7 → сервер M6", "### Идеи на разбор");
    let sid = "oz-ns-1";
    let ctx = context(&p.hook(
        "session-start",
        &p.ev("SessionStart", sid, json!({"source": "startup"})),
    ));
    assert!(ctx.contains("ZeroAIMemory"), "дайджест: {ctx}");
    assert!(
        ctx.len() <= 9000,
        "вывод старого клиента в пределе канала Claude Code: {}",
        ctx.len()
    );
    ok(
        &p.hook(
            "prompt",
            &p.ev(
                "UserPromptSubmit",
                sid,
                json!({"prompt": "почисти кэш миниатюр"}),
            ),
        ),
        "prompt",
    );
    // события M6 (`zam init` M6 вешает их на `zam hook checkpoint`), которых старый zam не
    // знает: код 0, вывод агенту пуст
    for event in ["SessionEnd", "PostCompact"] {
        let o = p.hook("checkpoint", &p.ev(event, sid, json!({"reason": "other"})));
        assert_eq!(o.status.code(), Some(0), "{event}: {}", err(&o));
        assert_eq!(out(&o).trim(), "", "{event}: вывод агенту");
    }
    // эпоха по source=compact (у старого клиента — только с [hooks] recovery = true)
    let o = p.hook(
        "session-start",
        &p.ev("SessionStart", sid, json!({"source": "compact"})),
    );
    ok(&o, "session-start compact");
    let (c, sess) = rest(&url, &tok, "GET", &format!("/sessions/{sid}"), None);
    assert_eq!(c, 200, "{sess}");
    assert_eq!(sess["seen_epoch"], 1, "эпоха после compact: {sess}");
    // карта кода старого клиента: recv_count — 0
    let (c, st) = rest_h(
        &url,
        &tok,
        "GET",
        "/code/status",
        None,
        &[("X-Zam-Solution", G)],
    );
    assert_eq!(c, 200, "{st}");
}

// ---------- §17 п. 15: p95 /recall(prompt) ----------

/// Предыдущий выпуск — база сравнения p95 `/recall(prompt)` (M7.1 §4.8 п. 3): тег и версия
/// `/health`. С каждым выпуском сдвигается на него.
const PREV_RELEASE: (&str, &str) = ("v0.0.13-m6.6", "0.0.13-m6.6");

/// Release `zam-server` предыдущего выпуска [`PREV_RELEASE`]: `ZAM_PREV_SERVER_BIN` или
/// `target/tags/<тег>/release/zam-server` (сборка тега — как у баз M6: `git archive` тега,
/// `ZAM_GIT_SHA`, `--target-dir target/tags/<тег>`).
fn prev_release_server() -> Option<PathBuf> {
    let p = std::env::var_os("ZAM_PREV_SERVER_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            common::repo_root()
                .join("target/tags")
                .join(PREV_RELEASE.0)
                .join("release")
                .join(exe("zam-server"))
        });
    if p.is_file() {
        Some(p)
    } else {
        eprintln!("нет {} (ZAM_PREV_SERVER_BIN) — пропуск", p.display());
        None
    }
}

/// Release `zam-server` M6: `ZAM_M6_SERVER_BIN` или `zam-server` рядом с бинарником теста.
fn m6_release_server() -> Option<PathBuf> {
    let p = std::env::var_os("ZAM_M6_SERVER_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join(exe("zam-server"))
        });
    if p.is_file() {
        Some(p)
    } else {
        eprintln!("нет {} (ZAM_M6_SERVER_BIN) — пропуск", p.display());
        None
    }
}

/// Мир замера в решении `G`: заметки, insight с триггерами, 50 идей человека.
fn load_world(s: &Srv) {
    let topics = ["кэш", "деплой", "поиск", "логирование", "миграции"];
    for i in 0..30 {
        let t = topics[i % 5];
        let (c, v) = rest(
            &s.url,
            &s.token,
            "POST",
            "/nodes",
            Some(
                &json!({"space": "knowledge", "path": format!("p95/{t}-{i}"), "kind": "note",
                         "content": format!("# {t} {i}\n\nЗаметка про {t}: шаг {i}, \
                                             настройка и проверка.\n")}),
            ),
        );
        assert!((200..300).contains(&c), "{v}");
    }
    for i in 0..10 {
        let t = topics[i % 5];
        let (c, v) = rest(
            &s.url,
            &s.token,
            "POST",
            &format!("/spaces/{G}/entries"),
            Some(
                &json!({"kind": "insight", "title": format!("Ловушка {t} {i}"),
                         "body": format!("При работе с {t} проверь шаг {i}."),
                         "data": {"kind": "gotcha"}}),
            ),
        );
        assert!((200..300).contains(&c), "{v}");
    }
    for i in 0..50 {
        let (c, v) = rest(
            &s.url,
            &s.token,
            "POST",
            &format!("/spaces/{G}/entries"),
            Some(
                &json!({"kind": "idea", "title": format!("Идея {i}: улучшить {}", topics[i % 5]),
                         "body": "Замысел на будущее.", "data": {"origin": "human"}}),
            ),
        );
        assert_eq!(c, 201, "{v}");
    }
}

/// Один вызов `/recall {event: prompt}` новой сессией: время ответа.
fn recall_prompt(s: &Srv, session: &str, text: &str) -> Duration {
    let t = Instant::now();
    let (c, v) = rest_h(
        &s.url,
        &s.token,
        "POST",
        "/recall",
        Some(&json!({"event": "prompt", "solution": G, "prompt": text})),
        &[("X-Zam-Session", session)],
    );
    let d = t.elapsed();
    assert_eq!(c, 200, "{v}");
    d
}

fn p95(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[(v.len() * 95).div_ceil(100) - 1]
}

/// §17 п. 15: p95 `/recall(prompt)` release-сервера ветки не хуже предыдущего выпуска + 10 %
/// ([`PREV_RELEASE`]; до M7.1 база — замороженная v0.0.10-m5.7, M7.1 §4.8 п. 3): один мир
/// (30 заметок, 10 insight, 50 идей) в двух каталогах, серверы без моделей; 5 раундов по 40
/// вызовов на сервер вперемешку (общий фон нагрузки), каждый вызов — новая сессия; критерий —
/// медиана по раундам отношения p95 ветка / предыдущий выпуск ≤ 1,10 (предрегистрация 13b).
/// Время — запрос по петле целиком глазами клиента (`ureq`). Запуск — в тихом окне и только
/// release (отладочный сервер против release-тега даёт отношение ≈ 3):
/// `cargo test -p zam-cli --release --test m6_acceptance acceptance_hook_budgets -- --ignored
/// --exact --nocapture --test-threads 1`.
#[test]
#[ignore = "замер времени для приёмки: release, тихое окно, бинарники ветки и предыдущего выпуска"]
fn acceptance_hook_budgets() {
    let _g = serial();
    let (Some(old), Some(new)) = (prev_release_server(), m6_release_server()) else {
        return;
    };
    let so = Srv::start(&old, "p95-prev");
    let sn = Srv::start(&new, "p95-new");
    assert_eq!(so.version(), PREV_RELEASE.1, "база — предыдущий выпуск");
    eprintln!("ZAM_INFO сервер ветки: {}", sn.version());
    load_world(&so);
    load_world(&sn);
    let prompts = [
        "как улучшить кэш и деплой, шаг 3",
        "почему поиск медленный после миграции",
        "настрой логирование для деплоя",
        "миграции базы падают на шаге 7",
        "добавь проверку кэша",
    ];
    for i in 0..10 {
        for s in [&so, &sn] {
            recall_prompt(s, &format!("warm-{i}"), prompts[i % 5]);
        }
    }
    const ROUNDS: usize = 5;
    const N: usize = 40;
    let mut ratios = Vec::new();
    for round in 0..ROUNDS {
        let (mut a, mut b) = (Vec::new(), Vec::new());
        for i in 0..N {
            let text = prompts[i % 5];
            // порядок в паре чередуется: ни один сервер не идёт всегда первым
            if i % 2 == 0 {
                a.push(recall_prompt(&so, &format!("r{round}-{i}"), text));
                b.push(recall_prompt(&sn, &format!("r{round}-{i}"), text));
            } else {
                b.push(recall_prompt(&sn, &format!("r{round}-{i}"), text));
                a.push(recall_prompt(&so, &format!("r{round}-{i}"), text));
            }
        }
        let (po, pn) = (p95(a.clone()), p95(b.clone()));
        let r = pn.as_secs_f64() / po.as_secs_f64();
        eprintln!(
            "ZAM_TIMING m6 p95 /recall(prompt) раунд {round}: {} p50 {:?} p95 {po:?}; ветка p50 \
             {:?} p95 {pn:?}; отношение {r:.3}",
            PREV_RELEASE.0,
            common::quantile(&a, 50),
            common::quantile(&b, 50)
        );
        ratios.push(r);
    }
    ratios.sort_by(f64::total_cmp);
    let med = ratios[ROUNDS / 2];
    eprintln!(
        "ZAM_TIMING m6 p95 /recall(prompt) к {}: медиана отношения {med:.3} (порог 1,10)",
        PREV_RELEASE.0
    );
    assert!(
        med <= 1.10,
        "p95 /recall(prompt) ветки хуже {} + 10 %: {ratios:?}",
        PREV_RELEASE.0
    );
}
