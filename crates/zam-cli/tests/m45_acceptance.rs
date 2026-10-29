//! Приёмка M4.5 «Рабочая память» (спецификация `docs/specs/2026-10-28-m4.5-working-memory.md`
//! §13, ТЗ §19, задача 16): сквозные сценарии против настоящего zam-server в процессе теста и
//! настоящего `zam` (бинарник пакета); хуки — в формате Claude Code и `--agent codex` там, где у
//! Codex есть событие (§10):
//!
//! - `compact_recovers_task_and_journal` — задача захвачена и в работе с `next_step`, правка
//!   файла, ошибка и её исправление; `SessionStart source=compact` выдаёт задачу и сводку
//!   журнала сессии (файл, ошибка, пара «ошибка → исправление»);
//! - `question_two_sides` — агент задаёт вопрос, человек отвечает в WebUI (REST сессии WebUI),
//!   следующий `session-start` показывает ответ, создано решение `axiom` с `derived_from`;
//!   второй вопрос агент записывает с ответом человека из чата (MCP) — тот же результат;
//! - `glossary_ru_en` — термин «Выжимка» с псевдонимом `summary`: поиск `summary` находит
//!   заметку со словом «выжимка» (канал `alias`), запись заметки со словом `summary` —
//!   предупреждение `term_alias`, подсказка термина к промпту;
//! - `file_change_marks_stale` — insight со ссылкой на `src/a.rs` (хэш), правка файла хуком →
//!   `POST /files/changed` → поиск помечает «возможно устарело», чтение файла — подсказка о
//!   записи без сети;
//! - `subagent_brief` — `pre-tool` на `Agent` с заданием про тему решения → `subagent-start`
//!   выдаёт пакет с нужным решением и термином в бюджете `recall.brief_tokens`;
//! - `vault_environment` — `host` с открытыми полями находится `vault?q=`, виден в дайджесте без
//!   секрета, секрет — только раскрытием с аудитом;
//! - `m4_db_migrates` — база пользователя M4 (миграция 0014) с данными переходит на 0018 без
//!   потерь, `foreign_key_check` пуст.
//!
//! Сервер — без моделей (поиск FTS-only), как у приёмки M4 (`m4_acceptance.rs`); каталоги — в
//! `CARGO_TARGET_TMPDIR`. Тесты идут по одному (`serial`).
mod common;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Output;
use std::time::Duration;

use assert_cmd::Command;
use serde_json::{Value, json};
use zam_core::{Id, TargetKind, TargetRef};
use zam_server::build;
use zam_server::config::ServerConfig;
use zam_store::{
    Access, Attribution, AuthorKind, Certainty, Db, EdgeType, EntryKind, EntryPatch, NewEntry,
    NewTrigger, TriggerKind, USER_MIGRATIONS, create_entry, delete_entry, supersede_entry,
    update_entry,
};

/// Решение проектов теста.
const GA: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e45";
/// Пароль администратора первого старта.
const PW: &str = "pw";

/// Тесты файла — по одному: хуки укладываются в бюджеты, соседи не должны их съедать.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn tmp(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("cli-m45-{name}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// HTTP-клиент теста без системного прокси; новый на каждый запрос — без пула соединений
/// (штатная остановка сервера не ждёт простаивающие keep-alive).
fn http() -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .proxy(None)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(60)))
            .build(),
    )
}

/// Запрос к REST: (код, тело текстом, `Set-Cookie`).
fn rest_raw(
    url: &str,
    headers: &[(&str, &str)],
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> (u16, String, Option<String>) {
    let a = http();
    let uri = format!("{url}{path}");
    let r = match method {
        "GET" => {
            let mut q = a.get(&uri);
            for (k, v) in headers {
                q = q.header(*k, *v);
            }
            q.call()
        }
        _ => {
            let mut q = match method {
                "POST" => a.post(&uri),
                "PATCH" => a.patch(&uri),
                m => panic!("метод {m}"),
            };
            for (k, v) in headers {
                q = q.header(*k, *v);
            }
            let payload = body.map(Value::to_string).unwrap_or_default();
            q.content_type("application/json").send(payload)
        }
    };
    let mut r = r.unwrap_or_else(|e| panic!("{method} {path}: {e}"));
    let code = r.status().as_u16();
    let cookie = r
        .headers()
        .get("set-cookie")
        .and_then(|c| c.to_str().ok())
        .map(|c| c.split(';').next().unwrap_or("").to_string());
    let text = r.body_mut().read_to_string().unwrap_or_default();
    (code, text, cookie)
}

/// Запрос к REST: (код, JSON или `Null`).
fn rest(
    url: &str,
    headers: &[(&str, &str)],
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let (code, text, _) = rest_raw(url, headers, method, path, body);
    let v = if text.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("{method} {path}: не JSON ({e}): {text}"))
    };
    (code, v)
}

/// Пользователь сервера: id и токен `rw`.
struct User {
    id: String,
    token: String,
}

impl User {
    fn auth(&self) -> String {
        format!("Bearer {}", self.token)
    }
}

/// Снимок базы M4 до миграции ([`fill_m4_db`]).
struct M4Snapshot {
    /// Путь файла базы пользователя.
    path: PathBuf,
    /// Строк в таблицах `entries`, `entry_revisions`, `edges`, `triggers`.
    counts: BTreeMap<String, i64>,
    /// Запись с уникальным словом в заголовке.
    marked: String,
    /// Запись, правленная в M4 (ревизия 2), с явной связью.
    revised: String,
}

/// Сервер в фоновом потоке со штатной остановкой. `users[0]` — администратор (первый токен,
/// вход в WebUI — `web`), далее — пользователи `names`.
struct Srv {
    url: String,
    users: Vec<User>,
    /// Cookie и CSRF сессии WebUI администратора.
    web: (String, String),
    m4: Option<M4Snapshot>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    done: Option<std::sync::mpsc::Receiver<()>>,
}

impl Srv {
    fn start(name: &str, tweak: impl FnOnce(&mut ServerConfig) + Send + 'static) -> Srv {
        Self::start_with(name, &[], false, tweak)
    }

    /// Сервер с пользователями `names`; `m4_db` — база первого из них до старта — база M4
    /// (0014) с данными ([`fill_m4_db`]).
    fn start_with(
        name: &str,
        names: &[&str],
        m4_db: bool,
        tweak: impl FnOnce(&mut ServerConfig) + Send + 'static,
    ) -> Srv {
        let data = tmp(&format!("{name}-data"));
        let names: Vec<String> = names.iter().map(|s| s.to_string()).collect();
        let (tx, rx) = std::sync::mpsc::channel();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let mut cfg = ServerConfig {
                    data_dir: data.clone(),
                    ..ServerConfig::default()
                };
                // отладочная сборка на HDD: сервер не обрезает сбор (бюджет — у клиента хука)
                cfg.recall.timeout_ms = 30_000;
                tweak(&mut cfg);
                let (app, st, rep) = build(cfg, Some(PW.into())).await.unwrap();
                let mut users = vec![User {
                    id: st.admin_id.to_string(),
                    token: rep.initial_token.expect("первый токен администратора"),
                }];
                let mut m4 = None;
                for n in &names {
                    let u = st.system.create_user(n, false).await.unwrap();
                    let (_, t) = st.system.issue_token(u.id, "t", Access::Rw).await.unwrap();
                    if m4_db && m4.is_none() {
                        // путь — как `DataDir::user_db` (каталог данных занят сервером)
                        let path = data.join("users").join(format!("{}.db", u.id));
                        m4 = Some(fill_m4_db(path, u.id).await);
                    }
                    users.push(User {
                        id: u.id.to_string(),
                        token: t,
                    });
                }
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let url = format!("http://{}", listener.local_addr().unwrap());
                tx.send((url, users, m4)).unwrap();
                axum::serve(
                    listener,
                    app.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .with_graceful_shutdown(async move {
                    let _ = stop_rx.await;
                })
                .await
                .unwrap();
            });
            let _ = done_tx.send(());
        });
        let (url, users, m4): (String, Vec<User>, Option<M4Snapshot>) = rx.recv().unwrap();
        let (code, text, cookie) = rest_raw(
            &url,
            &[],
            "POST",
            "/api/v1/auth/login",
            Some(&json!({"login": "admin", "password": PW})),
        );
        assert_eq!(code, 200, "{text}");
        let csrf = serde_json::from_str::<Value>(&text).unwrap()["csrf"]
            .as_str()
            .unwrap()
            .to_string();
        let s = Srv {
            url,
            users,
            web: (cookie.expect("cookie сессии WebUI"), csrf),
            m4,
            stop: Some(stop_tx),
            done: Some(done_rx),
        };
        s.warm_up();
        s
    }

    /// Прогрев: база пользователя и первые запросы отладочной сборки — до хуков с бюджетом.
    fn warm_up(&self) {
        for u in 0..self.users.len() {
            for _ in 0..2 {
                self.call(
                    u,
                    "POST",
                    "/api/v1/recall",
                    Some(&json!({"event": "session_start", "session_id": "warmup"})),
                    &[],
                );
                self.call(u, "GET", "/api/v1/sessions/warmup", None, &[]);
                self.call(u, "GET", "/api/v1/triggers", None, &[]);
            }
        }
    }

    /// Запрос пользователя `u` (`Authorization: Bearer`) с дополнительными заголовками.
    fn call(
        &self,
        u: usize,
        method: &str,
        path: &str,
        body: Option<&Value>,
        extra: &[(&str, &str)],
    ) -> (u16, Value) {
        let auth = self.users[u].auth();
        let mut h = vec![("Authorization", auth.as_str())];
        h.extend_from_slice(extra);
        rest(&self.url, &h, method, path, body)
    }

    /// Запрос из WebUI (cookie и CSRF сессии администратора).
    fn web(&self, method: &str, path: &str, body: Option<&Value>) -> (u16, Value) {
        let (cookie, csrf) = &self.web;
        rest(
            &self.url,
            &[("Cookie", cookie), ("X-CSRF-Token", csrf)],
            method,
            path,
            body,
        )
    }

    /// Регистрация решения `GA` пользователем `u`.
    fn register(&self, u: usize) {
        let (code, v) = self.call(
            u,
            "POST",
            "/api/v1/solutions",
            Some(&json!({"agent_solution_id": GA, "name": "Рабочая память"})),
            &[],
        );
        assert_eq!(code, 201, "{v}");
    }

    /// Запись журнала через REST в пространство `space`; id.
    fn entry(&self, u: usize, space: &str, body: Value) -> String {
        let (code, v) = self.call(
            u,
            "POST",
            &format!("/api/v1/spaces/{space}/entries"),
            Some(&body),
            &[("X-Zam-Solution", GA)],
        );
        assert_eq!(code, 201, "{v}");
        v["id"].as_str().unwrap().to_string()
    }

    fn get(&self, u: usize, path: &str) -> Value {
        let (code, v) = self.call(u, "GET", path, None, &[]);
        assert_eq!(code, 200, "GET {path}: {v}");
        v
    }

    /// Штатная остановка: сервер больше не принимает соединения.
    fn stop(&mut self) {
        if let Some(s) = self.stop.take() {
            let _ = s.send(());
        }
        if let Some(d) = self.done.take() {
            d.recv_timeout(Duration::from_secs(60))
                .expect("сервер не остановился за 60 с");
        }
        let addr: SocketAddr = self.url.trim_start_matches("http://").parse().unwrap();
        assert!(
            std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_err(),
            "сервер ещё принимает соединения"
        );
    }
}

impl Drop for Srv {
    fn drop(&mut self) {
        if let Some(s) = self.stop.take() {
            let _ = s.send(());
        }
        if let Some(d) = self.done.take() {
            let _ = d.recv_timeout(Duration::from_secs(30));
        }
    }
}

/// Инструмент MCP `/mcp` (streamable HTTP, JSON-ответ): текст результата; ошибка — паника.
fn mcp(srv: &Srv, u: usize, extra: &[(&str, &str)], name: &str, args: Value) -> String {
    let auth = srv.users[u].auth();
    let mut h = vec![
        ("Authorization", auth.as_str()),
        ("Accept", "application/json, text/event-stream"),
    ];
    h.extend_from_slice(extra);
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                      "params": {"name": name, "arguments": args}});
    let (code, v) = rest(&srv.url, &h, "POST", "/mcp", Some(&body));
    assert_eq!(code, 200, "{name}: {v}");
    assert!(v.get("error").is_none(), "{name}: ошибка JSON-RPC: {v}");
    let r = &v["result"];
    let text = r["content"][0]["text"].as_str().unwrap_or("").to_string();
    assert_ne!(r["isError"].as_bool(), Some(true), "{name}: {text}");
    text
}

/// Ссылка `<prefix>:<id>` из текста ответа.
fn ref_in(text: &str, prefix: &str) -> String {
    text.split_whitespace()
        .find_map(|w| w.strip_prefix(prefix))
        .map(|w| {
            w.trim_end_matches(|c: char| !c.is_ascii_alphanumeric())
                .to_string()
        })
        .unwrap_or_else(|| panic!("нет ссылки {prefix}… в: {text}"))
}

/// Проект агента: каталог с `.zeroaimemory/solution.toml` решения `GA` и «домашний» каталог.
struct Proj {
    url: String,
    token: String,
    repo: PathBuf,
    home: PathBuf,
}

impl Proj {
    fn new(name: &str, srv: &Srv, u: usize) -> Proj {
        let root = tmp(name);
        let repo = root.join("repo");
        let home = root.join("home");
        std::fs::create_dir_all(repo.join(".zeroaimemory")).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            repo.join(".zeroaimemory/solution.toml"),
            format!("agent_solution_id = \"{GA}\"\nname = \"Рабочая память\"\n"),
        )
        .unwrap();
        Proj {
            url: srv.url.clone(),
            token: srv.users[u].token.clone(),
            repo,
            home,
        }
    }

    fn cmd(&self) -> Command {
        let mut c = common::zam_cmd();
        c.env("ZAM_URL", &self.url)
            .env("ZAM_TOKEN", &self.token)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("COMPUTERNAME", "wm-box")
            .env("HOSTNAME", "wm-box")
            .current_dir(&self.repo);
        c
    }

    /// `zam <args>` с stdin; `agent` — запуск из сессии агента (`CLAUDECODE=1`: сессия хуков).
    fn zam(&self, agent: bool, args: &[&str], stdin: &str) -> Output {
        let mut c = self.cmd();
        if agent {
            c.env("CLAUDECODE", "1");
        }
        c.args(args)
            .write_stdin(stdin.as_bytes().to_vec())
            .output()
            .unwrap()
    }

    /// `zam hook <kind> --agent <agent>` с событием на stdin.
    fn hook_as(&self, agent: &str, kind: &str, v: &Value) -> Output {
        self.cmd()
            .args(["hook", kind, "--agent", agent])
            .write_stdin(v.to_string().into_bytes())
            .output()
            .unwrap()
    }

    fn hook(&self, kind: &str, v: &Value) -> Output {
        self.hook_as("claude", kind, v)
    }

    /// Событие с общими полями (`session_id`, `cwd`).
    fn ev(&self, event: &str, session: &str, extra: Value) -> Value {
        let mut v = json!({
            "session_id": session,
            "transcript_path": self.repo.join("t.jsonl"),
            "cwd": self.repo,
            "hook_event_name": event,
        });
        for (k, x) in extra.as_object().unwrap() {
            v[k] = x.clone();
        }
        v
    }

    fn start_ev(&self, session: &str, source: &str) -> Value {
        self.ev("SessionStart", session, json!({"source": source}))
    }

    /// Claude `PostToolUse` (успех) инструмента `tool`.
    fn post_ok(&self, session: &str, tool: &str, input: Value) -> Value {
        self.ev(
            "PostToolUse",
            session,
            json!({"tool_name": tool, "tool_use_id": "u9", "tool_input": input,
                   "tool_response": {"success": true}}),
        )
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.repo.join(".zeroaimemory/logs/hooks.log")).unwrap_or_default()
    }

    /// `session-start` (до 5 попыток: хук укладывается в бюджет или молчит — fail-open), пока
    /// дайджест не выполнит `done`; текст последнего непустого дайджеста.
    fn session_start_until(
        &self,
        agent: &str,
        session: &str,
        source: &str,
        done: impl Fn(&str) -> bool,
    ) -> String {
        let mut digest = String::new();
        for _ in 0..5 {
            let o = self.hook_as(agent, "session-start", &self.start_ev(session, source));
            if let Some(c) = context_opt(&o, "SessionStart") {
                digest = c;
            }
            if done(&digest) {
                return digest;
            }
        }
        panic!(
            "session-start ({agent}, {source}): дайджест «{digest}»; hooks.log:\n{}",
            self.log()
        );
    }
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// Код 0 и stdout.
fn ok(o: &Output) -> String {
    assert_eq!(
        o.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        out(o),
        String::from_utf8_lossy(&o.stderr)
    );
    out(o)
}

/// Id из вывода `zam log|task new|question ask` (`<id>@<rev>`).
fn id_of(o: &Output) -> String {
    ok(o).trim().split('@').next().unwrap().to_string()
}

/// `additionalContext` вывода хука (код 0, событие проверяется); пусто — `None`.
fn context_opt(o: &Output, event: &str) -> Option<String> {
    let s = ok(o);
    if s.trim().is_empty() {
        return None;
    }
    let v: Value = serde_json::from_str(s.trim()).unwrap_or_else(|e| panic!("не JSON ({e}): {s}"));
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], event, "{v}");
    Some(
        v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap_or_else(|| panic!("нет additionalContext: {v}"))
            .to_string(),
    )
}

fn context(o: &Output, event: &str) -> String {
    context_opt(o, event).unwrap_or_else(|| panic!("пустой вывод хука {event}"))
}

/// Оценка токенов без модели — как у сервера (`ApproxCounter`): ⌈2·символы/7⌉.
fn approx_tokens(s: &str) -> usize {
    (s.chars().count() * 2).div_ceil(7)
}

fn blake(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

// ---- 1. Сжатие ----

/// §13 п. 1: задача захвачена сессией и в работе с `next_step`, правка файла, ошибка сборки и
/// её исправление; после `PreCompact` пакет восстановления `SessionStart source=compact` несёт
/// задачу с `next_step` и сводку журнала (файл, ошибка, пара «ошибка → исправление»). То же —
/// у Codex (`PostToolUse` Bash со stderr, `apply_patch`).
#[test]
fn compact_recovers_task_and_journal() {
    let _g = serial();
    let srv = Srv::start("compact", |_| {});
    srv.register(0);
    let p = Proj::new("compact", &srv, 0);
    std::fs::create_dir_all(p.repo.join("src")).unwrap();
    std::fs::write(p.repo.join("src/cache.rs"), b"pub fn cache() {}\n").unwrap();
    const S: &str = "wm-c1";
    p.session_start_until("claude", S, "startup", |_| true);
    // агент: задача, захват сессией хуков, в работу со следующим шагом
    let tid = id_of(&p.zam(
        false,
        &[
            "task",
            "new",
            "--title",
            "Перенести кэш выжимок в SQLite",
            "--next",
            "схема таблицы",
        ],
        "План: схема, миграция, тесты.\n",
    ));
    let k = format!("k:{tid}");
    ok(&p.zam(true, &["task", "claim", &k, "--ttl", "60"], ""));
    ok(&p.zam(
        true,
        &[
            "task",
            "status",
            &k,
            "in_progress",
            "--next",
            "дописать миграцию 0019",
        ],
        "",
    ));
    // правка, ошибка сборки, правка, успешная сборка
    let edit = p.post_ok(S, "Edit", json!({"file_path": p.repo.join("src/cache.rs")}));
    let steps = [
        edit.clone(),
        p.ev(
            "PostToolUseFailure",
            S,
            json!({"tool_name": "Bash", "tool_input": {"command": "cargo build"},
                   "error": "error[E0425]: cannot find value `pool` in this scope"}),
        ),
        edit,
        p.post_ok(S, "Bash", json!({"command": "cargo build"})),
    ];
    for s in &steps {
        ok(&p.hook("post-tool", s));
    }
    // сжатие: PreCompact пишет событие журнала, вывода нет
    ok(&p.hook(
        "checkpoint",
        &p.ev("PreCompact", S, json!({"trigger": "auto"})),
    ));
    let task_line = format!(
        "ZeroAIMemory: [in_progress] Перенести кэш выжимок в SQLite — далее: дописать миграцию 0019 — {k}"
    );
    let c = p.session_start_until("claude", S, "compact", |c| c.contains("До сжатия"));
    let at = c
        .find(
            "ZeroAIMemory: До сжатия в этой сессии: правились src/cache.rs; ошибки: 1 (последняя: ",
        )
        .unwrap_or_else(|| panic!("нет сводки журнала: {c}"));
    assert!(c.contains("### Задачи"), "{c}");
    let t = c
        .find(&task_line)
        .unwrap_or_else(|| panic!("нет задачи: {c}"));
    assert!(t < at, "сервер — первым, сводка — в конце: {c}");
    assert!(
        c[at..].contains("cannot find value") && c[at..].contains("исправлены: "),
        "{c}"
    );
    assert!(
        c[at..].contains("→ src/cache.rs"),
        "пара «ошибка → исправление»: {c}"
    );

    // Codex: ошибка Bash — stderr с ненулевым кодом, правка — apply_patch
    const X: &str = "wm-cx1";
    p.session_start_until("codex", X, "startup", |_| true);
    let patch = "*** Begin Patch\n*** Update File: src/cache.rs\n@@\n-a\n+b\n*** End Patch\n";
    for v in [
        p.ev(
            "PostToolUse",
            X,
            json!({"tool_name": "Bash", "tool_input": {"command": "cargo test"},
                   "tool_response": {"stdout": "", "exit_code": 101,
                                     "stderr": "error[E0599]: no method named `get` found"}}),
        ),
        p.ev(
            "PostToolUse",
            X,
            json!({"tool_name": "apply_patch", "tool_input": {"command": patch},
                   "tool_response": "{\"stdout\":\"Success. Updated the following files\"}"}),
        ),
        p.ev(
            "PostToolUse",
            X,
            json!({"tool_name": "Bash", "tool_input": {"command": "cargo test"},
                   "tool_response": {"stdout": "test result: ok", "stderr": "",
                                     "exit_code": 0}}),
        ),
    ] {
        ok(&p.hook_as("codex", "post-tool", &v));
    }
    let c = p.session_start_until("codex", X, "compact", |c| c.contains("До сжатия"));
    assert!(c.contains(&task_line), "{c}");
    let at = c
        .find("ZeroAIMemory: До сжатия в этой сессии: правились src/cache.rs; ошибки: 1")
        .unwrap_or_else(|| panic!("нет сводки Codex: {c}"));
    assert!(
        c[at..].contains("no method named") && c[at..].contains("→ src/cache.rs"),
        "{c}"
    );
}

// ---- 2. Вопрос с двух сторон ----

/// Решение из ответа: `axiom`, `decision`, `decided_by: human`, ребро `derived_from` → вопрос.
fn assert_decision_from(srv: &Srv, did: &str, qid: &str) {
    let d = srv.get(0, &format!("/api/v1/entries/{did}"));
    assert_eq!(d["certainty"], "axiom", "{d}");
    assert_eq!(d["data"]["kind"], "decision", "{d}");
    assert_eq!(d["data"]["decision"]["decided_by"], "human", "{d}");
    let links = srv.get(0, &format!("/api/v1/entries/{did}/links"));
    let to_q = links["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["type"] == "derived_from" && e["dst"].as_str().unwrap().ends_with(qid))
        .count();
    assert_eq!(to_q, 1, "{links}");
}

/// §13 п. 2: агент задаёт вопрос (`zam question ask`), человек отвечает в WebUI — следующий
/// `session-start` (Claude и Codex) показывает ответ, создано решение `axiom` с `derived_from`;
/// второй вопрос агент задаёт через MCP и записывает ответ человека из чата — тот же результат.
#[test]
fn question_two_sides() {
    let _g = serial();
    let srv = Srv::start("question", |_| {});
    srv.register(0);
    let p = Proj::new("question", &srv, 0);
    const Q1: &str = "Какую базу взять для кэша выжимок?";
    let qid = id_of(&p.zam(
        true,
        &[
            "question",
            "ask",
            "--title",
            Q1,
            "--option",
            "a=SQLite",
            "--option",
            "b=Postgres",
            "--recommend",
            "a",
        ],
        "Кэш растёт, нужен выбор хранилища.\n",
    ));
    let c = p.session_start_until("claude", "wm-q1", "startup", |c| !c.is_empty());
    assert!(c.contains(&format!("1 открытых: «{Q1}»")), "{c}");
    // человек — в WebUI (сессия WebUI: by human, channel webui)
    let (code, j) = srv.web(
        "POST",
        &format!("/api/v1/entries/{qid}/answer"),
        Some(&json!({"option": "a", "text": "Postgres — лишний процесс"})),
    );
    assert_eq!(code, 200, "{j}");
    let did = j["decision_id"].as_str().unwrap().to_string();
    assert_eq!(j["entry"]["data"]["answer"]["channel"], "webui", "{j}");
    assert_eq!(j["entry"]["data"]["answer"]["by"], "human", "{j}");
    assert_decision_from(&srv, &did, &qid);
    let line = format!("ZeroAIMemory: ответ на «{Q1}»: ");
    let tail = format!("— q:{qid} → i:{did}");
    for (agent, s) in [("claude", "wm-q2"), ("codex", "wm-q2x")] {
        let c = p.session_start_until(agent, s, "startup", |c| c.contains(&tail));
        assert!(
            c.contains("### Ответы человека") && c.contains(&line),
            "{agent}: {c}"
        );
    }

    // второй вопрос: MCP (агент без хуков), ответ человека в чате записывает агент
    let h = [("X-Zam-Solution", GA), ("X-Zam-Session", "wm-q3")];
    const Q2: &str = "Включать ли реранк в хуках?";
    let t = mcp(
        &srv,
        0,
        &h,
        "memory_log",
        json!({"kind": "question", "title": Q2, "body": "Реранк точнее, но медленнее.",
               "data": {"options": [{"key": "yes", "text": "Да"}, {"key": "no", "text": "Нет"}]}}),
    );
    let q2 = ref_in(&t, "q:");
    mcp(
        &srv,
        0,
        &h,
        "memory_update_entry",
        json!({"id": format!("q:{q2}"), "action": "answer", "option": "no",
               "text": "Хуки обязаны укладываться в бюджет", "channel": "chat", "by": "human"}),
    );
    let q = srv.get(0, &format!("/api/v1/entries/{q2}"));
    assert_eq!(q["status"], "answered", "{q}");
    let a = &q["data"]["answer"];
    assert_eq!(
        (a["by"].as_str(), a["channel"].as_str()),
        (Some("human"), Some("chat")),
        "{q}"
    );
    let did2 = a["decision_id"].as_str().unwrap().to_string();
    assert_decision_from(&srv, &did2, &q2);
    let tail2 = format!("— q:{q2} → i:{did2}");
    let c = p.session_start_until("claude", "wm-q4", "startup", |c| c.contains(&tail2));
    assert!(
        c.contains(&format!("ZeroAIMemory: ответ на «{Q2}»: ")),
        "{c}"
    );
}

// ---- 3. Словарь ----

/// §13 п. 3: термин «Выжимка» с псевдонимом `summary` (`zam log term`): поиск `summary` находит
/// заметку со словом «выжимка» каналом `alias` (REST, `zam search`, MCP), запись заметки со
/// словом `summary` — предупреждение `term_alias`; промпт с `summary` — подсказка термина
/// (Claude и Codex), дайджест — строка словаря.
#[test]
fn glossary_ru_en() {
    let _g = serial();
    let srv = Srv::start("glossary", |_| {});
    srv.register(0);
    let p = Proj::new("glossary", &srv, 0);
    let (code, n) = srv.call(
        0,
        "POST",
        "/api/v1/nodes",
        Some(&json!({"space": GA, "path": "Кэш/Устройство", "kind": "note",
                     "content": "# Кэш ответов\n\nВыжимка записи хранится отдельно и отдаётся быстро.\n"})),
        &[],
    );
    assert_eq!(code, 201, "{n}");
    let nid = n["id"].as_str().unwrap().to_string();
    let search = || {
        let (code, j) = srv.call(
            0,
            "POST",
            "/api/v1/search",
            Some(&json!({"query": "summary", "scope": {"solution": GA},
                         "filters": {"kinds": ["note"]}})),
            &[],
        );
        assert_eq!(code, 200, "{j}");
        j
    };
    let j = search();
    assert!(
        j["items"].as_array().unwrap().is_empty(),
        "без словаря не находит: {j}"
    );
    let tid = id_of(&p.zam(
        true,
        &[
            "log",
            "term",
            "--title",
            "Выжимка",
            "--definition",
            "Краткое изложение найденного для агента",
            "--alias",
            "summary",
        ],
        "",
    ));
    let j = search();
    assert_eq!(j["items"][0]["node_id"], nid.as_str(), "{j}");
    assert!(
        j["items"][0]["sources"]
            .as_array()
            .unwrap()
            .contains(&json!("alias")),
        "канал alias: {j}"
    );
    assert_eq!(j["expanded"][0]["term"], "Выжимка", "{j}");
    let s = ok(&p.zam(true, &["search", "summary"], ""));
    assert!(s.contains("Кэш/Устройство"), "zam search: {s}");
    let h = [("X-Zam-Solution", GA)];
    let t = mcp(&srv, 0, &h, "memory_search", json!({"query": "summary"}));
    assert!(t.contains("Кэш/Устройство"), "memory_search: {t}");
    // заметка со словом-псевдонимом — предупреждение, запись не отклонена
    let t = mcp(
        &srv,
        0,
        &h,
        "memory_write",
        json!({"path": "Кэш/Заметка", "content": "# Про summary\n\nНужен summary ответа.\n"}),
    );
    assert!(t.contains("term_alias: «summary» → «Выжимка»"), "{t}");
    // подсказка термина к промпту и строка словаря в дайджесте
    let term_line = format!(
        "ZeroAIMemory: термин «Выжимка» — Краткое изложение найденного для агента — t:{tid}"
    );
    for (agent, s) in [("claude", "wm-g1"), ("codex", "wm-g2")] {
        let d = p.session_start_until(agent, s, "startup", |c| !c.is_empty());
        assert!(
            d.contains("Словарь: 1 терминов — memory_search kinds=[term]"),
            "{d}"
        );
        let pr = p.ev(
            "UserPromptSubmit",
            s,
            json!({"prompt": "как устроена summary в кэше?"}),
        );
        let c = context(&p.hook_as(agent, "prompt", &pr), "UserPromptSubmit");
        assert!(c.contains(&term_line), "{agent}: {c}");
    }
}

// ---- 4. Файл ----

/// §13 п. 4: insight со ссылкой на `src/a.rs` (хэш ставит `zam`); правка файла хуком →
/// очередь → `POST /files/changed` (`prompt`) → поиск помечает «возможно устарело»; после
/// обновления индекса (`session-start`) чтение файла без сети — подсказка о записи с пометкой
/// (Claude `PostToolUse Read`, Codex — `pre-tool` на `apply_patch`).
#[test]
fn file_change_marks_stale() {
    let _g = serial();
    let mut srv = Srv::start("file", |_| {});
    srv.register(0);
    let p = Proj::new("file", &srv, 0);
    std::fs::create_dir_all(p.repo.join("src")).unwrap();
    std::fs::write(p.repo.join("src/a.rs"), b"pub fn a() {}\n").unwrap();
    const TITLE: &str = "Функция a паникует на пустом входе";
    let iid = id_of(&p.zam(
        true,
        &[
            "log", "insight", "--kind", "gotcha", "--title", TITLE, "--ref", "src/a.rs",
        ],
        "Пустой срез — паника индексации.\n",
    ));
    let refs = |state: &str| {
        let idx = srv.get(0, &format!("/api/v1/file-refs?solution={GA}"));
        let r = idx["refs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["path"] == "src/a.rs")
            .unwrap_or_else(|| panic!("нет src/a.rs: {idx}"))
            .clone();
        assert_eq!(r["items"][0]["id"], format!("i:{iid}"), "{idx}");
        assert_eq!(r["items"][0]["state"], state, "{idx}");
        r
    };
    assert_eq!(refs("ok")["hash"], blake(b"pub fn a() {}\n"));
    p.session_start_until("claude", "wm-f1", "startup", |_| true);
    // правка: хук ставит путь в очередь, `prompt` отправляет её
    std::fs::write(p.repo.join("src/a.rs"), b"pub fn a() { todo!() }\n").unwrap();
    ok(&p.hook(
        "post-tool",
        &p.post_ok(
            "wm-f1",
            "Edit",
            json!({"file_path": p.repo.join("src/a.rs")}),
        ),
    ));
    let pr = p.ev(
        "UserPromptSubmit",
        "wm-f1",
        json!({"prompt": "что с функцией a"}),
    );
    ok(&p.hook("prompt", &pr));
    refs("stale");
    let (code, j) = srv.call(
        0,
        "POST",
        "/api/v1/search",
        Some(&json!({"query": "функция паникует", "scope": {"solution": GA}})),
        &[],
    );
    assert_eq!(code, 200, "{j}");
    let hit = j["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["title"] == TITLE)
        .unwrap_or_else(|| panic!("нет записи: {j}"));
    assert_eq!(hit["stale_refs"], json!(["src/a.rs"]), "{j}");
    let t = mcp(
        &srv,
        0,
        &[("X-Zam-Solution", GA)],
        "memory_search",
        json!({"query": "функция паникует"}),
    );
    assert!(t.contains("возможно устарело: изменился src/a.rs"), "{t}");
    // индекс знания о файлах обновляется на старте сессии; дальше — без сети
    p.session_start_until("claude", "wm-f2", "startup", |_| true);
    srv.stop();
    let read = p.post_ok(
        "wm-f3",
        "Read",
        json!({"file_path": p.repo.join("src/a.rs")}),
    );
    let c = context(&p.hook("post-tool", &read), "PostToolUse");
    assert_eq!(
        c,
        format!(
            "ZeroAIMemory: о файле src/a.rs в памяти: {TITLE} [insight, возможно устарело] — i:{iid}"
        )
    );
    let patch = "*** Begin Patch\n*** Update File: src/a.rs\n@@\n-a\n+b\n*** End Patch\n";
    let pre = p.ev(
        "PreToolUse",
        "wm-f4",
        json!({"tool_name": "apply_patch", "tool_input": {"command": patch}}),
    );
    let c = context(&p.hook_as("codex", "pre-tool", &pre), "PreToolUse");
    assert!(
        c.contains("о файле src/a.rs в памяти: ") && c.contains("возможно устарело"),
        "{c}"
    );
}

// ---- 5. Субагент ----

/// §13 п. 5: `pre-tool` на `Agent` с заданием про тему решения → `subagent-start` выдаёт пакет с
/// нужным решением и термином словаря, без решения на чужую тему, в бюджете
/// `recall.brief_tokens`; сессия субагента — с `parent_id`. Codex — тот же пакет.
#[test]
fn subagent_brief() {
    let _g = serial();
    // FTS-only: балл без модели не сравним с порогом векторного поиска
    let srv = Srv::start("subagent", |c| c.recall.prompt_min_score = 0.0);
    srv.register(0);
    let p = Proj::new("subagent", &srv, 0);
    let dec = srv.entry(
        0,
        GA,
        json!({"kind": "insight", "title": "Квазикэш выжимок хранится только в SQLite",
               "body": "Решение: квазикэш выжимок держим в SQLite, без внешних процессов.",
               "certainty": "axiom", "data": {"kind": "decision"}, "force": true}),
    );
    let other = srv.entry(
        0,
        GA,
        json!({"kind": "insight", "title": "Журнал сервера пишется строками JSON",
               "body": "Строки журнала — JSON для разбора.",
               "certainty": "axiom", "data": {"kind": "decision"}, "force": true}),
    );
    let tid = id_of(&p.zam(
        true,
        &[
            "log",
            "term",
            "--title",
            "Квазикэш",
            "--definition",
            "Кэш выжимок поиска на стороне сервера",
            "--alias",
            "quasicache",
        ],
        "",
    ));
    const S: &str = "wm-s1";
    p.session_start_until("claude", S, "startup", |_| true);
    let task = "Перенеси квазикэш выжимок в SQLite и проверь миграцию";
    let spawn = |t: &str| {
        p.ev(
            "PreToolUse",
            S,
            json!({"tool_name": "Agent", "tool_use_id": "u1",
                   "tool_input": {"description": "d", "prompt": task, "subagent_type": t}}),
        )
    };
    ok(&p.hook("pre-tool", &spawn("Plan")));
    let sub = |id: &str| {
        p.ev(
            "SubagentStart",
            S,
            json!({"agent_id": id, "agent_type": "Plan"}),
        )
    };
    let c = context(&p.hook("subagent-start", &sub("ag-1")), "SubagentStart");
    assert!(
        c.contains("### Решения") && c.contains(&format!("i:{dec}")),
        "{c}"
    );
    assert!(
        c.contains("### Термины") && c.contains(&format!("t:{tid}")),
        "{c}"
    );
    assert!(
        !c.contains(&format!("i:{other}")),
        "решение на чужую тему: {c}"
    );
    let n = approx_tokens(&c);
    assert!(
        n <= 800,
        "пакет {n} токенов > recall.brief_tokens = 800: {c}"
    );
    let s = srv.get(0, &format!("/api/v1/sessions/{S}:ag-1"));
    assert_eq!(
        (s["parent_id"].as_str(), s["agent_type"].as_str()),
        (Some(S), Some("Plan")),
        "{s}"
    );
    // Codex: тот же пакет (адресат additionalContext у Codex — ⚠️ проверить у владельца)
    ok(&p.hook("pre-tool", &spawn("Plan")));
    let c = context(
        &p.hook_as("codex", "subagent-start", &sub("ag-2")),
        "SubagentStart",
    );
    assert!(
        c.contains(&format!("i:{dec}")) && c.contains(&format!("t:{tid}")),
        "{c}"
    );
}

// ---- 6. Окружение ----

/// §13 п. 6: запись Vault `host` с открытыми полями и значением (`zam vault add`) находится
/// `vault?q=` и видна в дайджесте (Claude и Codex) без значения; значение — только раскрытием
/// (MCP `memory_vault get`) с аудитом `vault.reveal`.
#[test]
fn vault_environment() {
    let _g = serial();
    let srv = Srv::start("vault", |_| {});
    srv.register(0);
    let p = Proj::new("vault", &srv, 0);
    const SECRET: &str = "zv-wm-Qm9zcy1zZWNyZXQ-x7Lp2Rt9Wd";
    let vid = id_of(&p.zam(
        false,
        &[
            "vault",
            "add",
            "--title",
            "stage",
            "--type",
            "host",
            "--purpose",
            "acceptance stand",
            "--address",
            "10.0.0.5:22",
            "--limits",
            "nights only",
            "--secret-stdin",
            "--global",
        ],
        &format!("{SECRET}\n"),
    ));
    let (code, v) = srv.call(
        0,
        "GET",
        "/api/v1/spaces/knowledge/vault?q=acceptance",
        None,
        &[],
    );
    assert_eq!(code, 200, "{v}");
    let items = v["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{v}");
    assert_eq!(items[0]["title"], "stage", "{v}");
    assert!(!v.to_string().contains(SECRET), "значение в списке: {v}");
    let line = format!(
        "ZeroAIMemory: stage — acceptance stand; 10.0.0.5:22; ограничения: nights only — v:{vid}"
    );
    for (agent, s) in [("claude", "wm-v1"), ("codex", "wm-v2")] {
        let c = p.session_start_until(agent, s, "startup", |c| c.contains("### Окружение"));
        assert!(c.contains(&line), "{agent}: {c}");
        assert!(!c.contains(SECRET), "{agent}: значение в дайджесте");
    }
    let audit = |srv: &Srv| {
        let (code, a) = srv.web("GET", "/api/v1/admin/audit?limit=100", None);
        assert_eq!(code, 200, "{a}");
        a.to_string()
    };
    assert!(
        !audit(&srv).contains("vault.reveal"),
        "раскрытия ещё не было"
    );
    let t = mcp(
        &srv,
        0,
        &[("X-Zam-Session", "wm-v3")],
        "memory_vault",
        json!({"action": "get", "id": format!("v:{vid}")}),
    );
    assert!(t.contains(SECRET), "раскрытие: {t}");
    let a = audit(&srv);
    assert!(a.contains("vault.reveal"), "{a}");
    assert!(!a.contains(SECRET), "значение в аудите");
}

// ---- 7. Миграция базы M4 ----

/// База пользователя M4 (`USER_MIGRATIONS[..14]`) с данными M4: пространства, 30 insight
/// Knowledge (10 — с двумя триггерами), явные связи восьми типов M4 по кругу, правка каждой
/// пятой записи, три замещения и удалённая запись. Снимок строк таблиц — до миграции.
async fn fill_m4_db(path: PathBuf, user: Id) -> M4Snapshot {
    let db = Db::open(&path, &USER_MIGRATIONS[..14]).unwrap();
    let knowledge = Id::new();
    let (rules, human) = (Id::new(), Id::new());
    db.write(move |tx| {
        for (id, kind, name) in [
            (rules, "rules", "Rules"),
            (human, "human", "Human"),
            (knowledge, "knowledge", "Knowledge"),
        ] {
            tx.execute(
                &format!(
                    "INSERT INTO spaces (id, kind, name, created_at)
                     VALUES ('{id}', '{kind}', '{name}', '2026-10-27T00:00:00.000000Z')"
                ),
                [],
            )?;
        }
        Ok(())
    })
    .await
    .unwrap();
    let a = Attribution {
        author_kind: AuthorKind::Agent,
        token_id: Some(Id::new()),
        session_id: Some("s-m4".into()),
        machine: Some("m4-box".into()),
        message: None,
        user_id: Some(user),
        area: None,
        solution: None,
    };
    let explicit: Vec<EdgeType> = EdgeType::ALL
        .into_iter()
        .filter(|t| !matches!(t, EdgeType::Supersedes | EdgeType::Contradicts))
        .collect();
    let new = |title: String| NewEntry {
        refs: None,
        space_id: knowledge,
        kind: EntryKind::Insight,
        title,
        body: "Тело записи M4 для чанков и FTS.".into(),
        data: json!({"kind": "gotcha"}),
        area: None,
        tags: vec!["m4".into()],
        status: None,
        certainty: Some(Certainty::Hypothesis),
        idempotency_key: None,
        triggers: Vec::new(),
        links: Vec::new(),
    };
    let mut ids: Vec<Id> = Vec::new();
    let (mut marked, mut revised) = (None, None);
    for i in 0..30usize {
        let title = if i == 7 {
            "Квазиуникальный заголовок миграции M4".to_string()
        } else {
            format!("Ловушка M4 номер {i}")
        };
        let mut e = new(title);
        if i < 10 {
            e.triggers = vec![
                NewTrigger {
                    kind: TriggerKind::Keyword,
                    pattern: format!("слово{i}"),
                    tools: None,
                },
                NewTrigger {
                    kind: TriggerKind::FileGlob,
                    pattern: format!("src/**/m{i}.rs"),
                    tools: Some(vec!["Edit".into()]),
                },
            ];
        }
        if let Some(prev) = ids.last() {
            let dst = TargetRef {
                kind: TargetKind::Entry,
                id: *prev,
            };
            e.links = vec![(explicit[ids.len() % explicit.len()], dst)];
        }
        let (c, _) = create_entry(&db, e, &a).await.unwrap();
        if i % 5 == 0 {
            let patch = EntryPatch {
                append_body: Some(format!("Дополнение {i}.")),
                ..EntryPatch::default()
            };
            update_entry(&db, c.id, Some(c.rev), patch, &a)
                .await
                .unwrap();
            if i == 5 {
                revised = Some(c.id);
            }
        }
        if i == 7 {
            marked = Some(c.id);
        }
        ids.push(c.id);
    }
    for old in ids.iter().skip(20).take(3) {
        let mut e = new(format!("Замена {old}"));
        // база v14: таблицы `file_refs` ещё нет — ссылки не наследуются (пустой набор явно)
        e.refs = Some(Vec::new());
        supersede_entry(&db, *old, None, e, &a).await.unwrap();
    }
    delete_entry(&db, ids[25], None, &a).await.unwrap();
    let counts = table_counts(&db).await;
    let version: u32 = db
        .read(|c| Ok(c.query_row("PRAGMA user_version", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(version, 14, "база M4 до старта сервера");
    M4Snapshot {
        path,
        counts,
        marked: marked.unwrap().to_string(),
        revised: revised.unwrap().to_string(),
    }
}

/// Строк в таблицах записей журналов, ревизий, связей и триггеров.
async fn table_counts(db: &Db) -> BTreeMap<String, i64> {
    let mut m = BTreeMap::new();
    for t in ["entries", "entry_revisions", "edges", "triggers"] {
        let n: i64 = db
            .read(move |c| Ok(c.query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get(0))?))
            .await
            .unwrap();
        m.insert(t.to_string(), n);
    }
    m
}

/// §13 п. 7: база пользователя M4 (0014) открывается сервером M4.5 и переходит на 0018 без
/// потери данных: записи, правки, связи, триггеры и FTS на месте, новые виды записываются,
/// хук получает дайджест и триггеры; копия `bak-v14` есть, `foreign_key_check` пуст.
#[test]
fn m4_db_migrates() {
    let _g = serial();
    let mut srv = Srv::start_with("m4db", &["Анна"], true, |_| {});
    let snap = srv.m4.take().expect("база M4");
    srv.register(1);
    // запись M4 по id, правка и связь
    let e = srv.get(1, &format!("/api/v1/entries/{}", snap.revised));
    assert_eq!(e["rev"], 2, "{e}");
    assert!(e["body"].as_str().unwrap().contains("Дополнение 5."), "{e}");
    let links = srv.get(1, &format!("/api/v1/entries/{}/links", snap.revised));
    assert!(!links["items"].as_array().unwrap().is_empty(), "{links}");
    // FTS по заголовку записи M4
    let (code, j) = srv.call(
        1,
        "POST",
        "/api/v1/search",
        Some(&json!({"query": "квазиуникальный", "filters": {"kinds": ["insight"]}})),
        &[],
    );
    assert_eq!(code, 200, "{j}");
    assert!(
        j["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["entry_id"] == snap.marked.as_str()),
        "{j}"
    );
    // триггеры M4 и дайджест — хуку агента
    let (code, t) = srv.call(1, "GET", "/api/v1/triggers", None, &[]);
    assert_eq!(code, 200, "{t}");
    assert!(t.to_string().contains("src/**/m3.rs"), "{t}");
    let p = Proj::new("m4db", &srv, 1);
    let d = p.session_start_until("claude", "wm-m4", "startup", |c| !c.is_empty());
    assert!(d.contains("Ловушка M4"), "{d}");
    // новые виды пишутся в мигрированную базу
    let term = srv.entry(
        1,
        "knowledge",
        json!({"kind": "term", "title": "Пересборка", "body": "",
               "data": {"definition": "Миграция с копированием таблицы", "aliases": ["rebuild"]}}),
    );
    srv.stop();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let db = Db::open(&snap.path, USER_MIGRATIONS).unwrap();
        let (version, violations, fk, term_kind) = db
            .read(move |c| {
                let v: u32 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
                let mut st = c.prepare("PRAGMA foreign_key_check")?;
                let n = st.query_map([], |_| Ok(()))?.count();
                let fk: bool = c.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
                let k: String =
                    c.query_row("SELECT kind FROM entries WHERE id = ?1", [&term], |r| {
                        r.get(0)
                    })?;
                Ok((v, n, fk, k))
            })
            .await
            .unwrap();
        assert_eq!(version, USER_MIGRATIONS.len() as u32);
        assert!(version >= 18);
        assert_eq!(violations, 0, "foreign_key_check не пуст");
        assert!(fk, "foreign_keys выключены");
        assert_eq!(term_kind, "term");
        // строки M4 на месте: + термин (запись и его первая ревизия), связи и триггеры — те же
        let after = table_counts(&db).await;
        let b = &snap.counts;
        assert_eq!(after["entries"], b["entries"] + 1, "{b:?} → {after:?}");
        assert_eq!(
            after["entry_revisions"],
            b["entry_revisions"] + 1,
            "{b:?} → {after:?}"
        );
        assert_eq!(after["edges"], b["edges"], "{b:?} → {after:?}");
        assert_eq!(after["triggers"], b["triggers"], "{b:?} → {after:?}");
    });
    let bak = snap
        .path
        .with_file_name(format!("{}.db.bak-v14", srv.users[1].id));
    assert!(bak.exists(), "{}", bak.display());
}
