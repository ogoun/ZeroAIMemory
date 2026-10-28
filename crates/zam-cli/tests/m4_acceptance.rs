//! Приёмка M4 (ТЗ §19, задача 16): сквозные сценарии «агент → память → хуки» против настоящего
//! zam-server в процессе теста и настоящего `zam` (бинарник пакета):
//!
//! - `crlf_scenario` — **e2e CRLF**: агент через MCP пишет insight `gotcha` (`scope=global`,
//!   триггеры `file_glob` и `error_regex`) на одной машине в проекте A; на другой машине в проекте B
//!   `zam hook session-start` обновляет кэш триггеров, сервер останавливается, и `pre-tool`
//!   (Write `deploy.sh`) и `post-tool` («bad interpreter») выдают подсказки без сети;
//! - `hook_budgets` — p95 каждого хука на корпусе eval в пределах бюджетов §10.3 с запасом ×2;
//! - `source_change_marks_cache_stale` — изменение источника выжимки → `stale` (критерий §19);
//! - `fail_open_when_server_down` — все пять хуков при выключенном и при зависшем сервере;
//! - `shared_solution_cache_and_recall` — участник общего решения видит общий кэш и подсказки
//!   решения и свой Knowledge, не видит чужой; после отзыва — ничего из решения.
//!
//! Тест лежит в пакете `zam-cli`, а не `zam-server` (как в плане): бинарник `zam` доступен
//! тестам только своего пакета (`CARGO_BIN_EXE_zam`), а `zam-cli` уже зависит от
//! `zam-server` как от библиотеки **[решение исполнителя, задача 16]**.
//!
//! Сервер — без моделей (поиск FTS-only): цифры с bge-m3 на CPU сняты `cargo xtask eval --hints`
//! (задача 15) и записаны в журнал этапа. Каталоги — в `CARGO_TARGET_TMPDIR`.
use std::io::Read as _;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, Instant};

use assert_cmd::Command;
use serde_json::{Value, json};
use zam_server::build;
use zam_server::config::ServerConfig;
use zam_store::Access;

/// Решение проекта A (и общего решения в `shared_solution_cache_and_recall`).
const GA: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9ea1";
/// Решение проекта B.
const GB: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9ea2";
/// Адрес, где никто не слушает: эфемерный порт занимается и сразу освобождается (fix round 1,
/// M-3 — порт 9 мог бы кто-то слушать).
fn dead_url() -> String {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    drop(l);
    url
}
/// Пароль администратора первого старта.
const PW: &str = "pw";
/// Хуки — по одному: замеры бюджетов не должны зависеть от соседних тестов.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Бюджет хука по ТЗ §10.3, мс.
fn budget_ms(kind: &str) -> u64 {
    match kind {
        "session-start" => 300,
        "prompt" | "post-tool" => 250,
        "pre-tool" => 50,
        "checkpoint" => 100,
        other => panic!("неизвестный хук {other}"),
    }
}

/// Запас к бюджету в CI (общие раннеры, отладочная сборка): ×2.
const CI_MARGIN: u64 = 2;
/// Предел времени хука без сервера или с зависшим сервером: бюджет ×4 (как `hooks.rs`), не
/// меньше 400 мс — запуск процесса отладочной сборки под нагрузкой.
fn fail_open_max(kind: &str) -> Duration {
    Duration::from_millis((budget_ms(kind) * 4).max(400))
}

fn tmp(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("cli-m4-{name}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// HTTP-клиент теста без системного прокси; новый на каждый запрос — без пула соединений
/// (иначе штатная остановка сервера ждала бы простаивающие keep-alive).
fn http() -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .proxy(None)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(60)))
            .build(),
    )
}

/// Запрос к REST: `method` — GET | POST | PATCH | PUT | DELETE; ответ — (код, JSON или `Null`).
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

/// То же, что [`rest`]: (код, тело текстом, `Set-Cookie`).
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
        "GET" | "DELETE" => {
            let mut q = if method == "GET" {
                a.get(&uri)
            } else {
                a.delete(&uri)
            };
            for (k, v) in headers {
                q = q.header(*k, *v);
            }
            q.call()
        }
        _ => {
            let mut q = match method {
                "POST" => a.post(&uri),
                "PATCH" => a.patch(&uri),
                "PUT" => a.put(&uri),
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

/// Пользователь сервера: id (base32) и токен `rw`.
struct User {
    id: String,
    token: String,
}

impl User {
    fn auth(&self) -> String {
        format!("Bearer {}", self.token)
    }
}

/// Сервер в фоновом потоке со штатной остановкой (`stop`).
struct Srv {
    url: String,
    users: Vec<User>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    done: Option<std::sync::mpsc::Receiver<()>>,
}

impl Srv {
    /// Сервер с пользователями `names` (токены `rw`); `tweak` — правка конфигурации.
    fn start(
        name: &str,
        names: &[&str],
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
                    data_dir: data,
                    ..ServerConfig::default()
                };
                tweak(&mut cfg);
                let (app, st, _) = build(cfg, Some(PW.into())).await.unwrap();
                let mut users = Vec::new();
                for n in &names {
                    let u = st.system.create_user(n, false).await.unwrap();
                    let (_, t) = st.system.issue_token(u.id, "t", Access::Rw).await.unwrap();
                    users.push(User {
                        id: u.id.to_string(),
                        token: t,
                    });
                }
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let url = format!("http://{}", listener.local_addr().unwrap());
                tx.send((url, users)).unwrap();
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
        let (url, users): (String, Vec<User>) = rx.recv().unwrap();
        let s = Srv {
            url,
            users,
            stop: Some(stop_tx),
            done: Some(done_rx),
        };
        s.warm_up();
        s
    }

    /// Прогрев: база пользователя и первые запросы отладочной сборки — до хуков с бюджетом.
    fn warm_up(&self) {
        for u in &self.users {
            let h = [("Authorization", u.auth())];
            let h: Vec<(&str, &str)> = h.iter().map(|(k, v)| (*k, v.as_str())).collect();
            for _ in 0..2 {
                rest(
                    &self.url,
                    &h,
                    "POST",
                    "/api/v1/recall",
                    Some(&json!({"event": "session_start", "session_id": "warmup"})),
                );
                rest(&self.url, &h, "GET", "/api/v1/sessions/warmup", None);
                rest(&self.url, &h, "GET", "/api/v1/triggers", None);
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

    /// Регистрация решения `guid` пользователем `u`: код ответа (201 — новое, 202 — запрос).
    fn register(&self, u: usize, guid: &str, name: &str) -> u16 {
        self.call(
            u,
            "POST",
            "/api/v1/solutions",
            Some(&json!({"agent_solution_id": guid, "name": name})),
            &[],
        )
        .0
    }

    /// Insight через REST в пространстве `space`; id.
    fn insight(&self, u: usize, space: &str, title: &str, kind: &str, triggers: Value) -> String {
        let (code, v) = self.call(
            u,
            "POST",
            &format!("/api/v1/spaces/{space}/entries"),
            Some(&json!({"kind": "insight", "title": title,
                         "body": format!("{title}: разобрано на практике."),
                         "data": {"kind": kind}, "triggers": triggers, "force": true})),
            &[],
        );
        assert_eq!(code, 201, "{v}");
        v["id"].as_str().unwrap().to_string()
    }

    /// Заметка через REST; id.
    fn note(&self, u: usize, space: &str, path: &str, content: &str) -> String {
        let (code, v) = self.call(
            u,
            "POST",
            "/api/v1/nodes",
            Some(&json!({"space": space, "path": path, "kind": "note", "content": content})),
            &[],
        );
        assert_eq!(code, 201, "{v}");
        v["id"].as_str().unwrap().to_string()
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
    /// Сервер теста не переживает тест: фоновые записи соседа не мешают замерам.
    fn drop(&mut self) {
        if let Some(s) = self.stop.take() {
            let _ = s.send(());
        }
        if let Some(d) = self.done.take() {
            let _ = d.recv_timeout(Duration::from_secs(30));
        }
    }
}

/// Инструмент MCP `/mcp` (streamable HTTP, JSON-ответ): текст результата и `isError`.
fn mcp(url: &str, user: &User, extra: &[(&str, &str)], name: &str, args: Value) -> (String, bool) {
    let auth = user.auth();
    let mut h = vec![
        ("Authorization", auth.as_str()),
        ("Accept", "application/json, text/event-stream"),
    ];
    h.extend_from_slice(extra);
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                      "params": {"name": name, "arguments": args}});
    let (code, v) = rest(url, &h, "POST", "/mcp", Some(&body));
    assert_eq!(code, 200, "{name}: {v}");
    assert!(v.get("error").is_none(), "{name}: ошибка JSON-RPC: {v}");
    let r = &v["result"];
    (
        r["content"][0]["text"].as_str().unwrap_or("").to_string(),
        r["isError"].as_bool() == Some(true),
    )
}

/// Успешный вызов инструмента MCP — текст.
fn mcp_ok(url: &str, user: &User, extra: &[(&str, &str)], name: &str, args: Value) -> String {
    let (t, err) = mcp(url, user, extra, name, args);
    assert!(!err, "{name}: {t}");
    t
}

/// Ссылка `<prefix>:<id>` из текста ответа (`i:`, `n:`, `c_`).
fn ref_in(text: &str, prefix: &str) -> String {
    text.split_whitespace()
        .find_map(|w| w.strip_prefix(prefix))
        .map(|w| {
            w.trim_end_matches(|c: char| !c.is_ascii_alphanumeric())
                .to_string()
        })
        .unwrap_or_else(|| panic!("нет ссылки {prefix}… в: {text}"))
}

/// Проект агента: каталог с `.zeroaimemory/solution.toml`, «домашний» каталог, имя машины.
struct Proj {
    url: String,
    token: String,
    machine: String,
    repo: PathBuf,
    home: PathBuf,
}

impl Proj {
    fn new(name: &str, url: &str, token: &str, solution: &str, machine: &str) -> Proj {
        let root = tmp(name);
        let repo = root.join("repo");
        let home = root.join("home");
        std::fs::create_dir_all(repo.join(".zeroaimemory")).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            repo.join(".zeroaimemory/solution.toml"),
            format!("agent_solution_id = \"{solution}\"\nname = \"{name}\"\n"),
        )
        .unwrap();
        Proj {
            url: url.into(),
            token: token.into(),
            machine: machine.into(),
            repo,
            home,
        }
    }

    fn cmd(&self, url: &str) -> Command {
        let mut c = Command::cargo_bin("zam").unwrap();
        for (k, _) in std::env::vars().filter(|(k, _)| k.starts_with("ZAM_")) {
            c.env_remove(k);
        }
        for k in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "NO_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
            "no_proxy",
        ] {
            c.env_remove(k);
        }
        // окружение агента, в котором идут сами тесты, не должно утекать в `zam` (тот же список,
        // что у smoke комплекта и eval хуков)
        for k in zam_core::AGENT_ENV_VARS {
            c.env_remove(k);
        }
        c.env("ZAM_URL", url)
            .env("ZAM_TOKEN", &self.token)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("COMPUTERNAME", &self.machine)
            .env("HOSTNAME", &self.machine)
            .current_dir(&self.repo);
        c
    }

    /// `zam hook <kind>` (Claude Code) по адресу `url`: вывод и время процесса.
    fn hook_at(&self, url: &str, kind: &str, v: &Value) -> (Output, Duration) {
        let mut c = self.cmd(url);
        c.args(["hook", kind])
            .write_stdin(v.to_string().into_bytes());
        let t = Instant::now();
        let o = c.output().unwrap();
        (o, t.elapsed())
    }

    fn hook(&self, kind: &str, v: &Value) -> Output {
        self.hook_at(&self.url, kind, v).0
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

    fn start_ev(&self, session: &str) -> Value {
        self.ev("SessionStart", session, json!({"source": "startup"}))
    }

    fn write_ev(&self, session: &str, file: &str) -> Value {
        self.ev(
            "PreToolUse",
            session,
            json!({"tool_name": "Write", "tool_use_id": "u1",
                   "tool_input": {"file_path": self.repo.join(file), "content": "#!/bin/sh\n"}}),
        )
    }

    fn fail_ev(&self, session: &str, error: &str) -> Value {
        self.ev(
            "PostToolUseFailure",
            session,
            json!({"tool_name": "Bash", "tool_input": {"command": "./deploy.sh"}, "error": error}),
        )
    }

    fn triggers_path(&self) -> PathBuf {
        self.repo.join(".zeroaimemory/cache/triggers.json")
    }

    /// `entry_id` триггеров локального кэша (пусто — кэша нет).
    fn cached_entries(&self) -> Vec<String> {
        let Ok(s) = std::fs::read_to_string(self.triggers_path()) else {
            return Vec::new();
        };
        let v: Value = serde_json::from_str(&s).unwrap();
        v["triggers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["entry_id"].as_str().unwrap().to_string())
            .collect()
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.repo.join(".zeroaimemory/logs/hooks.log")).unwrap_or_default()
    }

    /// `session-start` до выполнения `done(кэш триггеров)` (≤ 5 попыток: хук укладывается в бюджет
    /// 300 мс или молчит — fail-open); текст последнего непустого дайджеста.
    fn session_start_until(&self, session: &str, done: impl Fn(&[String]) -> bool) -> String {
        let mut digest = String::new();
        for _ in 0..5 {
            let o = self.hook("session-start", &self.start_ev(session));
            if let Some(c) = context_opt(&o, "SessionStart") {
                digest = c;
            }
            if done(&self.cached_entries()) && !digest.is_empty() {
                return digest;
            }
        }
        panic!(
            "session-start: кэш {:?}, дайджест «{digest}»; hooks.log:\n{}",
            self.cached_entries(),
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

fn empty(o: &Output) {
    assert_eq!(ok(o).trim(), "", "ожидалась тишина");
}

/// Строка `hooks.log` о хуке, замолчавшем по сроку: внутренний тайм-аут HTTP или
/// `recall timeout` сервера.
fn is_timeout(line: &str) -> bool {
    line.contains("timeout") || line.contains("бюджет исчерпан")
}

fn p95(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[(v.len() * 95).div_ceil(100) - 1]
}

// ---- e2e CRLF ----

/// Критерий M4 «e2e CRLF»: опыт, записанный агентом через MCP в одном проекте на одной машине,
/// срабатывает подсказкой в другом проекте на другой машине — локально, без сети.
#[test]
fn crlf_scenario() {
    let _g = serial();
    let mut srv = Srv::start("crlf", &["Анна"], |_| {});
    assert_eq!(srv.register(0, GA, "Проект A"), 201);
    assert_eq!(srv.register(0, GB, "Проект B"), 201);
    // машина A, проект A: агент пишет опыт через MCP (scope=global — в Knowledge)
    let text = mcp_ok(
        &srv.url,
        &srv.users[0],
        &[
            ("X-Zam-Solution", GA),
            ("X-Zam-Machine", "machine-a"),
            ("X-Zam-Session", "crlf-a"),
        ],
        "memory_log",
        json!({
            "kind": "insight",
            "title": "Скрипты *.sh — только LF",
            "body": "Скрипт с CRLF падает с «/bin/bash^M: bad interpreter»: сохранять *.sh с LF, \
                     в .gitattributes — `*.sh eol=lf`.",
            "data": {"kind": "gotcha"},
            "scope": "global",
            "triggers": [
                {"type": "file_glob", "pattern": "**/*.sh"},
                {"type": "error_regex", "pattern": "\\^M|bad interpreter"},
            ],
        }),
    );
    let id = ref_in(&text, "i:");
    // атрибуция записи A (fix round 1, M-4): машина A, сессия A, а сессия — в решении A
    let (code, e) = srv.call(0, "GET", &format!("/api/v1/entries/{id}"), None, &[]);
    assert_eq!(code, 200, "{e}");
    assert_eq!(e["author"]["machine"], "machine-a", "{e}");
    assert_eq!(e["author"]["session_id"], "crlf-a", "{e}");
    let (code, s) = srv.call(0, "GET", "/api/v1/sessions/crlf-a", None, &[]);
    assert_eq!(code, 200, "{s}");
    assert_eq!(s["machine"], "machine-a", "{s}");
    assert_eq!(s["solution"], GA, "{s}");

    // машина B, проект B: старт сессии обновляет кэш триггеров
    let b = Proj::new("crlf-b", &srv.url, &srv.users[0].token, GB, "machine-b");
    let digest = b.session_start_until("crlf-b-1", |c| c.contains(&id));
    assert!(
        digest.contains(&format!("i:{id}")),
        "ловушка Knowledge — в дайджесте: {digest}"
    );
    let cache: Value =
        serde_json::from_str(&std::fs::read_to_string(b.triggers_path()).unwrap()).unwrap();
    assert_eq!(cache["solution"], GB, "{cache}");
    let mine: Vec<&Value> = cache["triggers"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["entry_id"] == id.as_str())
        .collect();
    assert_eq!(mine.len(), 2, "оба триггера записи: {cache}");
    assert!(
        mine.iter().all(|t| t["space"] == "knowledge"),
        "scope=global — Knowledge: {cache}"
    );
    let (code, s) = srv.call(0, "GET", "/api/v1/sessions/crlf-b-1", None, &[]);
    assert_eq!(code, 200, "{s}");
    assert_eq!(s["machine"], "machine-b", "{s}");
    assert_eq!(s["solution"], GB, "{s}");

    // сеть пропала: сервер остановлен после sync
    srv.stop();
    let (o, d) = b.hook_at(&b.url, "pre-tool", &b.write_ev("crlf-b-1", "deploy.sh"));
    let c = context(&o, "PreToolUse");
    assert!(
        c.starts_with("ZeroAIMemory: для **/*.sh: Скрипты *.sh — только LF [gotcha, hypothesis]"),
        "{c}"
    );
    assert!(c.contains(&format!("i:{id}")), "{c}");
    assert!(d <= fail_open_max("pre-tool"), "pre-tool без сети: {d:?}");
    let (o, d) = b.hook_at(
        &b.url,
        "post-tool",
        &b.fail_ev(
            "crlf-b-1",
            "Exit code 126\n/bin/bash^M: bad interpreter: No such file or directory",
        ),
    );
    let c = context(&o, "PostToolUseFailure");
    assert!(
        c.contains(&format!(
            "ZeroAIMemory: эта ошибка уже разбиралась: Скрипты *.sh — только LF [gotcha, \
             hypothesis] — i:{id}"
        )),
        "{c}"
    );
    assert!(d <= fail_open_max("post-tool"), "post-tool без сети: {d:?}");
    // недоступность сервера — в журнале хуков, без текста ошибки инструмента
    let log = b.log();
    assert!(log.contains("post-tool"), "{log}");
    assert!(!log.contains("bad interpreter"), "{log}");
}

// ---- бюджеты хуков ----

/// Корпус eval в памяти пользователя 0: заметки `eval/corpus` в Knowledge, insight
/// `eval/insights.toml` и `eval/hints.toml` (с триггерами) в решении `GA` и Knowledge, правила
/// `eval/hints.toml` в Rules. Возвращает набор `eval/hints.toml`.
fn load_eval(srv: &Srv) -> toml::Table {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../eval");
    assert_eq!(srv.register(0, GA, "eval"), 201);
    let mut stack = vec![root.join("corpus")];
    let mut n = 0;
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "md") {
                let rel = p
                    .strip_prefix(root.join("corpus"))
                    .unwrap()
                    .with_extension("");
                let rel = rel.to_string_lossy().replace('\\', "/");
                let text = std::fs::read_to_string(&p).unwrap().replace("\r\n", "\n");
                srv.note(0, "knowledge", &rel, &text);
                n += 1;
            }
        }
    }
    let read = |f: &str| -> toml::Table {
        std::fs::read_to_string(root.join(f))
            .unwrap()
            .parse()
            .unwrap()
    };
    let hints = read("hints.toml");
    let mut m = 0;
    for set in [read("insights.toml"), hints.clone()] {
        for i in set["insight"].as_array().unwrap() {
            let space = if i.get("scope").and_then(|v| v.as_str()) == Some("solution") {
                GA
            } else {
                "knowledge"
            };
            let triggers = i
                .get("triggers")
                .map(|t| serde_json::to_value(t).unwrap())
                .unwrap_or(json!([]));
            let (code, v) = srv.call(
                0,
                "POST",
                &format!("/api/v1/spaces/{space}/entries"),
                Some(&json!({
                    "kind": "insight",
                    "title": i["title"].as_str().unwrap(),
                    "body": i["body"].as_str().unwrap(),
                    "data": {"kind": i["kind"].as_str().unwrap()},
                    "triggers": triggers,
                    "force": true,
                })),
                &[],
            );
            assert_eq!(code, 201, "{v}");
            m += 1;
        }
    }
    // Rules пишет человек (`rules.agent_write = false`): администратор разрешает запись токеном
    let (code, text, cookie) = rest_raw(
        &srv.url,
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
    let cookie = cookie.expect("cookie сессии администратора");
    let (code, v) = rest(
        &srv.url,
        &[("Cookie", &cookie), ("X-CSRF-Token", &csrf)],
        "PATCH",
        "/api/v1/admin/settings",
        Some(&json!({"rules": {"agent_write": true}})),
    );
    assert_eq!(code, 200, "{v}");
    let rules = hints["rule"].as_array().unwrap();
    for r in rules {
        srv.note(
            0,
            "rules",
            r["path"].as_str().unwrap(),
            &r["text"].as_str().unwrap().replace("\r\n", "\n"),
        );
    }
    eprintln!(
        "ZAM_TIMING корпус: {n} заметок, {m} insight, {} правил",
        rules.len()
    );
    hints
}

/// Критерий M4 «бюджеты хуков» (§10.3): каждый хук — процесс `zam hook …` на корпусе eval.
/// Главный критерий — не больше 5 % вызовов, замолчавших по внутреннему сроку хука (1× бюджета:
/// HTTP обрывается на «бюджет − 30 мс» от старта процесса); p95 времени процесса ≤ бюджета ×2
/// (общие раннеры CI, отладочная сборка) содержателен для локального `pre-tool`, у сетевых хуков
/// время и так ограничено сроком. Хук должен сделать работу: в окне вызова в `hooks.log` нет
/// других строк (401/403/5xx, разбор, конфигурация), ответ есть там, где он ожидается
/// (дайджест, `decision: block`, подсказки `pre-tool` и `error_regex` по разметке
/// `eval/hints.toml`), иначе быстрый отказ прошёл бы замер. Цифры машины владельца (release,
/// bge-m3 int8 на CPU) — в журнале этапа.
#[test]
fn hook_budgets() {
    let _g = serial();
    let srv = Srv::start("budgets", &["Анна"], |_| {});
    let hints = load_eval(&srv);
    let p = Proj::new("budgets", &srv.url, &srv.users[0].token, GA, "bench-box");
    // кэш триггеров — до pre-tool
    p.session_start_until("bud-warm", |c| !c.is_empty());
    // прогрев (fix round 1, M-2): после массовой загрузки корпуса отдельные `session-start`
    // упирались в бюджет (запись сессии ждала диск, занятый фоновой записью на HDD, — выбросы
    // шли и через 5–10 с после загрузки) — замер начинается, когда 20 вызовов подряд проходят
    // без тайм-аута и быстрее половины бюджета; предел ожидания — 120 с
    let t = Instant::now();
    let half = Duration::from_millis(budget_ms("session-start") / 2);
    let (mut streak, mut warm) = (0, 0);
    while streak < 20 {
        assert!(
            t.elapsed() < Duration::from_secs(120),
            "сервер не прогрелся за 120 с ({warm} вызовов); hooks.log:
{}",
            p.log()
        );
        let before = p.log().lines().count();
        let (_, d) = p.hook_at(
            &p.url,
            "session-start",
            &p.start_ev(&format!("bud-warm-{warm}")),
        );
        warm += 1;
        let late = d > half || p.log().lines().skip(before).any(is_timeout);
        streak = if late { 0 } else { streak + 1 };
    }
    eprintln!(
        "ZAM_TIMING прогрев: {warm} session-start за {:?}",
        t.elapsed()
    );
    let cases = hints["case"].as_array().unwrap();
    let inputs = |event: &str| -> Vec<&toml::Value> {
        cases
            .iter()
            .filter(|c| c["event"].as_str() == Some(event))
            .collect()
    };
    // (вход, ожидание ответа): `Some(true)` — должен ответить, `Some(false)` — молчать,
    // `None` — по качеству поиска (FTS-only), не проверяется по отдельному вызову
    let expect = |c: &toml::Value| -> Option<bool> {
        let has = c["expected"].as_array().is_some_and(|e| !e.is_empty());
        match c["event"].as_str() {
            // pre-tool детерминирован (маски и regex по кэшу): точно по разметке
            Some("pre_tool") => Some(has),
            // error_regex — детерминированное совпадение
            Some("post_tool")
                if c.get("note")
                    .and_then(|n| n.as_str())
                    .is_some_and(|n| n.contains("error_regex")) =>
            {
                Some(true)
            }
            _ => None,
        }
    };
    type Run<'a> = (&'a str, Vec<(Value, Option<bool>)>);
    let mut runs: Vec<Run> = Vec::new();
    runs.push((
        "session-start",
        (0..40)
            .map(|i| (p.start_ev(&format!("bud-ss-{i}")), Some(true)))
            .collect(),
    ));
    runs.push((
        "prompt",
        inputs("prompt")
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let ev = p.ev(
                    "UserPromptSubmit",
                    &format!("bud-pr-{i}"),
                    json!({"prompt": c["input"].as_str().unwrap()}),
                );
                (ev, expect(c))
            })
            .collect(),
    ));
    runs.push((
        "post-tool",
        inputs("post_tool")
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let ev = p.fail_ev(&format!("bud-pt-{i}"), c["input"].as_str().unwrap());
                (ev, expect(c))
            })
            .collect(),
    ));
    runs.push((
        "pre-tool",
        inputs("pre_tool")
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let input = c["input"].as_str().unwrap();
                let tool = c["tool"].as_str().unwrap();
                let ti = if tool == "Bash" {
                    json!({"command": input})
                } else {
                    json!({"file_path": p.repo.join(input), "content": "x"})
                };
                let ev = p.ev(
                    "PreToolUse",
                    &format!("bud-pre-{i}"),
                    json!({"tool_name": tool, "tool_input": ti}),
                );
                (ev, expect(c))
            })
            .collect(),
    ));
    runs.push((
        "checkpoint",
        (0..40)
            .map(|i| {
                let ev = p.ev(
                    "Stop",
                    &format!("bud-cp-{i}"),
                    json!({"stop_hook_active": false}),
                );
                // правка в сессии и ни одной записи в память — напоминание обязательно
                (ev, Some(true))
            })
            .collect(),
    ));
    let mut report = Vec::new();
    let mut fails = Vec::new();
    for (kind, evs) in &runs {
        if *kind == "checkpoint" {
            // правка в сессии — иначе checkpoint молчит без запроса к серверу
            for i in 0..evs.len() {
                p.hook("pre-tool", &p.write_ev(&format!("bud-cp-{i}"), "a.txt"));
            }
        }
        // прогрев вида (не в замер): после загрузки корпуса первые запросы отладочного сервера
        // читают холодные страницы SQLite с HDD — бюджет относится к рабочему серверу
        for (ev, _) in evs.iter().take(3) {
            let mut w = ev.clone();
            w["session_id"] = json!(format!("warm-{}", ev["session_id"].as_str().unwrap()));
            p.hook_at(&p.url, kind, &w);
        }
        let mut times = Vec::new();
        let (mut timeouts, mut spoke) = (0, 0);
        for (ev, want) in evs {
            let before = p.log().lines().count();
            let (o, d) = p.hook_at(&p.url, kind, ev);
            let said = !ok(&o).trim().is_empty();
            spoke += usize::from(said);
            times.push(d);
            let lines: Vec<String> = p.log().lines().skip(before).map(String::from).collect();
            let late = lines.iter().any(|l| is_timeout(l));
            timeouts += usize::from(late);
            // любая другая строка журнала — отказ, а не замер (I-1)
            for l in lines.iter().filter(|l| !is_timeout(l)) {
                fails.push(format!("{kind} {}: ошибка хука: {l}", ev["session_id"]));
            }
            // ответ, где он ожидается; вызов, замолчавший по сроку, учтён в тайм-аутах
            if let Some(w) = want
                && !late
                && said != *w
            {
                fails.push(format!(
                    "{kind} {}: ожидался {}, вывод «{}»",
                    ev["session_id"],
                    if *w { "ответ" } else { "тишина" },
                    out(&o).trim()
                ));
            }
        }
        let n = times.len();
        let max = *times.iter().max().unwrap();
        let mut sorted = times.clone();
        sorted.sort();
        let q = p95(times);
        let limit = Duration::from_millis(budget_ms(kind) * CI_MARGIN);
        report.push(format!(
            "{kind} n={n} p50={:?} p95={q:?} max={max:?} ответов={spoke} тайм-аутов={timeouts} \
             (бюджет {} мс)",
            sorted[n / 2],
            budget_ms(kind)
        ));
        if q > limit {
            fails.push(format!("{kind}: p95 {q:?} > {limit:?}"));
        }
        if timeouts * 20 > n {
            fails.push(format!("{kind}: тайм-аутов {timeouts} из {n} (> 5 %)"));
        }
        if spoke == 0 {
            fails.push(format!("{kind}: ни одного ответа из {n}"));
        }
    }
    eprintln!("ZAM_TIMING m4 hook_budgets: {}", report.join("; "));
    assert!(
        fails.is_empty(),
        "{}\n{}\nhooks.log:\n{}",
        fails.join("\n"),
        report.join("\n"),
        p.log()
    );
}

// ---- изменение источника → stale ----

/// Критерий M4 «изменение источника → `stale`»: агент через MCP пишет заметку, сохраняет
/// выжимку по ней (`memory_cache_put`), заметка меняется — выжимка `stale` с причиной, в поиске —
/// строка `[cache, stale: …]` без текста выжимки.
#[test]
fn source_change_marks_cache_stale() {
    let _g = serial();
    let srv = Srv::start("stale", &["Анна"], |_| {});
    let u = &srv.users[0];
    let h = [("X-Zam-Session", "stale-1"), ("X-Zam-Machine", "box")];
    const Q: &str = "как деплоим parser-сервис";
    const SUMMARY: &str = "Сборка multi-stage, для arm64 — buildx.";
    let path = "knowledge:/Деплой/parser";
    let t = mcp_ok(
        &srv.url,
        u,
        &h,
        "memory_write",
        json!({"path": path, "content": "# parser\n\nСборка multi-stage, для arm64 нужен buildx.\n"}),
    );
    assert!(t.contains("rev1"), "{t}");
    let n = ref_in(&t, "n:");
    let t = mcp_ok(
        &srv.url,
        u,
        &h,
        "memory_cache_put",
        json!({"query": Q, "summary": SUMMARY, "sources": [{"id": format!("n:{n}"), "rev": 1}]}),
    );
    let cid = format!("c_{}", ref_in(&t, "c_"));
    let t = mcp_ok(&srv.url, u, &h, "memory_search", json!({"query": Q}));
    assert!(t.starts_with("[cache]"), "{t}");
    assert!(t.contains(SUMMARY), "{t}");
    // источник меняется — выжимка устаревает
    let t = mcp_ok(
        &srv.url,
        u,
        &h,
        "memory_write",
        json!({"path": path, "content": "# parser\n\nСборка однослойная, buildx не нужен.\n",
               "base_rev": 1}),
    );
    assert!(t.contains("rev2"), "{t}");
    let (code, g) = srv.call(0, "GET", &format!("/api/v1/cache/{cid}"), None, &[]);
    assert_eq!(code, 200, "{g}");
    assert_eq!(g["state"], "stale", "{g}");
    assert_eq!(
        g["stale"],
        json!([{"target": format!("n:{n}"), "reason": "changed"}]),
        "{g}"
    );
    let t = mcp_ok(&srv.url, u, &h, "memory_search", json!({"query": Q}));
    let first = t.lines().next().unwrap_or("");
    assert!(
        first.starts_with(&format!("[cache, stale: n:{n} (changed)] «{Q}»")),
        "{t}"
    );
    assert!(
        !t.contains(SUMMARY),
        "текст устаревшей выжимки не отдаётся: {t}"
    );
}

// ---- fail-open ----

/// Сервер, который принимает соединения и никогда не отвечает (зависший процесс).
fn hanging_server() -> String {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for s in l.incoming().flatten() {
            // соединение держится открытым: запрос прочитан, ответа нет
            let mut s2 = s.try_clone().unwrap();
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                while matches!(s2.read(&mut buf), Ok(n) if n > 0) {}
            });
            held.push(s);
        }
    });
    url
}

/// Критерий M4 «fail-open хуков»: все пять хуков при выключенном (соединение отвергнуто) и
/// зависшем (соединение есть, ответа нет) сервере — пустой вывод, код 0, время в пределе;
/// агент никогда не блокируется. Кэш триггеров при этом не портится.
#[test]
fn fail_open_when_server_down() {
    let _g = serial();
    let hang = hanging_server();
    let dead = dead_url();
    for (label, url) in [("down", dead.as_str()), ("hang", hang.as_str())] {
        let p = Proj::new(&format!("fail-{label}"), url, "zam_x", GA, "box");
        let s = format!("fo-{label}");
        // правка в сессии: checkpoint идёт на сервер (GET /sessions), а не молчит сразу
        let (o, d) = p.hook_at(url, "pre-tool", &p.write_ev(&s, "a.txt"));
        empty(&o);
        assert!(d <= fail_open_max("pre-tool"), "{label} pre-tool: {d:?}");
        let cases = [
            ("session-start", p.start_ev(&s)),
            (
                "prompt",
                p.ev(
                    "UserPromptSubmit",
                    &s,
                    json!({"prompt": "секретный промпт пользователя"}),
                ),
            ),
            ("pre-tool", p.write_ev(&s, "deploy.sh")),
            ("post-tool", p.fail_ev(&s, "/bin/bash^M: bad interpreter")),
            (
                "checkpoint",
                p.ev("Stop", &s, json!({"stop_hook_active": false})),
            ),
        ];
        for (kind, ev) in &cases {
            let (o, d) = p.hook_at(url, kind, ev);
            empty(&o);
            assert!(d <= fail_open_max(kind), "{label} {kind}: {d:?}");
        }
        assert!(
            !p.triggers_path().exists(),
            "{label}: кэш триггеров не создаётся"
        );
        let log = p.log();
        for kind in ["session-start", "prompt", "post-tool", "checkpoint"] {
            assert!(
                log.contains(kind),
                "{label}: нет строки {kind} в hooks.log:\n{log}"
            );
        }
        assert!(!log.contains("секретный промпт"), "{log}");
        assert!(!log.contains("bad interpreter"), "{log}");
    }
}

// ---- общее решение: кэш и подсказки, отзыв ----

/// Критерий M4 «общий кэш и подсказки общего решения с отзывом доступа»: участник (Борис) видит
/// выжимку и подсказки решения владельца (Анна) и свой Knowledge, но не Knowledge владельца;
/// после отзыва — ничего из решения (кэш, дайджест, триггеры, подсказки).
#[test]
fn shared_solution_cache_and_recall() {
    let _g = serial();
    let srv = Srv::start("shared", &["Анна", "Борис"], |_| {});
    let (anna, boris) = (0, 1);
    const Q: &str = "как деплоим parser-сервис";
    const SUMMARY: &str = "Деплой parser: сборка multi-stage, для arm64 — buildx.";
    assert_eq!(srv.register(anna, GA, "Общее"), 201);
    let note = srv.note(
        anna,
        GA,
        "Деплой",
        "# Деплой\n\nСборка multi-stage, для arm64 нужен buildx.\n",
    );
    let sol = srv.insight(
        anna,
        GA,
        "Скрипты деплоя — только LF",
        "gotcha",
        json!([{"type": "file_glob", "pattern": "deploy/**/*.sh"},
               {"type": "error_regex", "pattern": "deploy-lock busy"}]),
    );
    let anna_private = srv.insight(
        anna,
        "knowledge",
        "Личная ловушка Анны",
        "gotcha",
        json!([{"type": "file_glob", "pattern": "**/*.anna"},
               {"type": "error_regex", "pattern": "anna-private-error"}]),
    );
    let (code, c) = srv.call(
        anna,
        "POST",
        "/api/v1/cache",
        Some(&json!({"query": Q, "summary": SUMMARY, "solution": GA,
                     "sources": [{"id": format!("n:{note}")}]})),
        &[],
    );
    assert_eq!(code, 201, "{c}");
    // участник: запрос доступа → владелец одобряет rw
    assert_eq!(srv.register(boris, GA, "Общее"), 202);
    let (code, v) = srv.call(
        anna,
        "POST",
        &format!(
            "/api/v1/solutions/{GA}/access/{}/approve",
            srv.users[boris].id
        ),
        Some(&json!({"access": "rw"})),
        &[],
    );
    assert_eq!(code, 200, "{v}");
    let own = srv.insight(
        boris,
        "knowledge",
        "Ловушка Бориса",
        "gotcha",
        json!([{"type": "file_glob", "pattern": "**/*.boris"}]),
    );

    let b = Proj::new(
        "shared-b",
        &srv.url,
        &srv.users[boris].token,
        GA,
        "boris-pc",
    );
    let digest = b.session_start_until("sh-1", |c| c.contains(&sol) && c.contains(&own));
    assert!(digest.contains(&format!("i:{sol}")), "{digest}");
    assert!(digest.contains(&format!("i:{own}")), "{digest}");
    assert!(
        !digest.contains(&anna_private),
        "чужой Knowledge в дайджесте: {digest}"
    );
    assert!(!digest.contains("Личная ловушка"), "{digest}");
    let cached = b.cached_entries();
    assert!(
        !cached.contains(&anna_private),
        "чужой Knowledge в кэше триггеров: {cached:?}"
    );
    // подсказки решения — локально и по ошибке
    let c = context(
        &b.hook("pre-tool", &b.write_ev("sh-1", "deploy/ci/run.sh")),
        "PreToolUse",
    );
    assert!(c.contains(&format!("i:{sol}")), "{c}");
    empty(&b.hook("pre-tool", &b.write_ev("sh-1", "notes/x.anna")));
    let c = context(
        &b.hook(
            "post-tool",
            &b.fail_ev("sh-1", "error: deploy-lock busy, retry later"),
        ),
        "PostToolUseFailure",
    );
    assert!(c.contains(&format!("i:{sol}")), "{c}");
    let o = b.hook(
        "post-tool",
        &b.fail_ev("sh-1", "fatal: anna-private-error happened"),
    );
    assert!(!ok(&o).contains(&anna_private), "{}", out(&o));
    // общий кэш: выжимка владельца — участнику первым элементом поиска по решению, с автором
    let hdr = [("X-Zam-Solution", GA), ("X-Zam-Session", "sh-1")];
    let t = mcp_ok(
        &srv.url,
        &srv.users[boris],
        &hdr,
        "memory_search",
        json!({"query": Q}),
    );
    assert!(t.starts_with("[cache]"), "{t}");
    assert!(t.contains(SUMMARY), "{t}");
    assert!(t.contains("автор Анна"), "{t}");
    let (code, l) = srv.call(
        boris,
        "GET",
        &format!("/api/v1/cache?solution={GA}"),
        None,
        &[],
    );
    assert_eq!(code, 200, "{l}");
    assert_eq!(l["items"].as_array().unwrap().len(), 1, "{l}");

    // отзыв доступа
    let (code, v) = srv.call(
        anna,
        "POST",
        &format!(
            "/api/v1/solutions/{GA}/access/{}/revoke",
            srv.users[boris].id
        ),
        None,
        &[],
    );
    assert_eq!(code, 200, "{v}");
    let digest = b.session_start_until("sh-2", |c| !c.contains(&sol) && c.contains(&own));
    assert!(!digest.contains(&format!("i:{sol}")), "{digest}");
    assert!(
        digest.contains(&format!("i:{own}")),
        "свой Knowledge остаётся: {digest}"
    );
    let o = b.hook("pre-tool", &b.write_ev("sh-2", "deploy/ci/run.sh"));
    empty(&o);
    let o = b.hook(
        "post-tool",
        &b.fail_ev("sh-2", "error: deploy-lock busy, retry later"),
    );
    assert!(!ok(&o).contains(&sol), "{}", out(&o));
    let (code, r) = srv.call(
        boris,
        "POST",
        "/api/v1/recall",
        Some(&json!({"event": "session_start", "solution": GA})),
        &[],
    );
    assert_eq!(code, 200, "{r}");
    assert!(!r["text"].as_str().unwrap().contains(&sol), "{r}");
    assert!(
        r["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().is_some_and(|w| w.contains("revoked"))),
        "{r}"
    );
    let hdr = [("X-Zam-Solution", GA), ("X-Zam-Session", "sh-2")];
    let (t, _) = mcp(
        &srv.url,
        &srv.users[boris],
        &hdr,
        "memory_search",
        json!({"query": Q}),
    );
    assert!(!t.contains(SUMMARY), "{t}");
    assert!(!t.contains("[cache"), "{t}");
    let (code, _) = srv.call(
        boris,
        "GET",
        &format!("/api/v1/cache?solution={GA}"),
        None,
        &[],
    );
    assert_eq!(code, 403);
}
