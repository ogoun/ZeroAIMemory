//! Приёмка M4 (ТЗ §19, задача 16): сквозные сценарии «агент → память → хуки» против настоящего
//! zam-server в процессе теста и настоящего `zam` (бинарник пакета):
//!
//! - `crlf_scenario` — **e2e CRLF**: агент через MCP пишет insight `gotcha` (`scope=global`,
//!   триггеры `file_glob` и `error_regex`) на одной машине в проекте A; на другой машине в проекте B
//!   `zam hook session-start` обновляет кэш триггеров, сервер останавливается, и `pre-tool`
//!   (Write `deploy.sh`) и `post-tool` («bad interpreter») выдают подсказки без сети;
//! - `hook_budgets` — p95 каждого хука на корпусе eval в пределах бюджетов §10.3 с запасом ×2
//!   (M4.5: и `post-tool` успеха — 50 мс без сети, и `subagent-start` — 300 мс; M5.7 — с 50
//!   идеями в решении: раздел `### Идеи` и отдельный поиск идей к `prompt`; M6 §12 —
//!   `checkpoint` с `hooks.checkpoint_impact = true`: `POST /code/impact` по правке файла со
//!   знанием параллельно с `GET /sessions/{id}`; `session-start` контрольной группы A/B —
//!   `hooks.holdout = 0.5`, сессии из группы: метка `POST /sessions {holdout}` до `/recall` в
//!   том же бюджете, вывод — тишина);
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
mod common;
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
        "session-start" | "subagent-start" | "session-start-holdout" | "session-start-catchup" => {
            300
        }
        "prompt" | "post-tool" => 250,
        // M4.5: `post-tool` успеха — весь процесс без сети (план M4.5, Global Constraints)
        "pre-tool" | "post-tool-ok" => 50,
        "checkpoint" | "checkpoint-snap" => 100,
        other => panic!("неизвестный хук {other}"),
    }
}

/// Запас к бюджету в CI (общие раннеры, отладочная сборка): ×2.
const CI_MARGIN: u64 = 2;
/// Предел времени хука **внутри процесса** без сервера или с зависшим сервером — у лучшего из
/// повторов ([`Proj::hook_best`]): полтора бюджета §10.3, как `max_inner` в `hooks.rs` (срок HTTP
/// — бюджет минус резерв, возврат ожидания сокета — с шагом таймера ОС, затем строки
/// `hooks.log`). Ловит хук, который ждёт мёртвый сервер дольше своего срока: второй срок запроса
/// сверх первого давал 1,86 бюджета (дефект 17b-http), прежний предел «×4 процесса целиком» его
/// пропускал.
fn fail_open_max(kind: &str) -> Duration {
    Duration::from_millis(budget_ms(kind) * 3 / 2)
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
        std::fs::create_dir_all(home.join(".zeroaimemory")).unwrap();
        // хуки приёмки без фоновой `zam code sync` (M5.6, ревью задачи 6 M8): бюджеты хуков
        // меряются без синхронизации карты за тот же тестовый сервер
        std::fs::write(
            home.join(".zeroaimemory/config.toml"),
            "[code]\nauto = false\n",
        )
        .unwrap();
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
        let mut c = common::zam_cmd();
        c.env("ZAM_URL", url)
            .env("ZAM_TOKEN", &self.token)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("COMPUTERNAME", &self.machine)
            .env("HOSTNAME", &self.machine)
            .current_dir(&self.repo);
        c
    }

    /// `zam hook <kind>` (Claude Code) по адресу `url` с бюджетом продукта: вывод и времена
    /// вызова ([`common::HookTimes`]: процесс целиком и, где есть канал, внутри процесса).
    fn hook_times(&self, url: &str, kind: &str, v: &Value) -> (Output, common::HookTimes) {
        let mut c = self.cmd(url);
        c.args(["hook", kind])
            .write_stdin(v.to_string().into_bytes());
        common::hook_timed(c, kind)
    }

    /// [`Proj::hook_times`]: вывод и время процесса целиком.
    fn hook_at(&self, url: &str, kind: &str, v: &Value) -> (Output, Duration) {
        let (o, t) = self.hook_times(url, kind, v);
        (o, t.outer)
    }

    /// Одиночная проверка срока, устойчивая к остановке диска: вызов повторяется (до трёх раз),
    /// пока время хука внутри процесса не уложится в `max`; вывод — первого вызова, время —
    /// лучшее. Остановка диска случайна и повтором снимается; хук, который в срок не укладывается,
    /// не уложится все три раза. Только там, где повтор события безвреден.
    fn hook_best(&self, url: &str, kind: &str, v: &Value, max: Duration) -> (Output, Duration) {
        let (first, t) = self.hook_times(url, kind, v);
        let mut best = t.hook();
        for _ in 0..2 {
            if best <= max {
                break;
            }
            best = best.min(self.hook_times(url, kind, v).1.hook());
        }
        (first, best)
    }

    /// Пустой вызов `zam hook <kind>` (stdin пуст — хук сразу выходит): время процесса целиком —
    /// мера запуска и выхода exe на этой машине в этот момент.
    fn idle(&self, kind: &str) -> Duration {
        let mut c = self.cmd(&self.url);
        c.args(["hook", kind]).write_stdin(Vec::new());
        let t = Instant::now();
        let _ = c.output().unwrap();
        t.elapsed()
    }

    /// Хук ради содержимого — со
    /// щедрым бюджетом (`ZAM_TEST_HOOK_BUDGET_MS`): критерий приёмки — содержимое, а с бюджетом
    /// продукта отладочная сборка на занятой машине молчит (fail-open); время и бюджеты проверяют
    /// `hooks` и release `hook_budgets` (ревью 17b-http M6). Замеры
    /// времени и fail-open — [`Proj::hook_at`], с бюджетом продукта.
    fn hook(&self, kind: &str, v: &Value) -> Output {
        self.cmd(&self.url)
            .env("ZAM_TEST_HOOK_BUDGET_MS", "10000")
            // снимок сессии (M6 §8а): его сроки — тоже бюджет продукта, содержимому не нужны
            .env("ZAM_TEST_SNAPSHOT_MS", "5000")
            .env("ZAM_TEST_SNAPSHOT_PUT_MS", "8000")
            .args(["hook", kind])
            .write_stdin(v.to_string().into_bytes())
            .output()
            .unwrap()
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
    line.contains("timeout")
        || line.contains("бюджет исчерпан")
        // очередь изменённых файлов не ушла: бюджет вышел до запроса (диск, занятая машина)
        || line.contains("остаток бюджета меньше")
        // снимок сессии (M6 §8а): срок `PUT` и поток снимка, не успевший к выходу хука
        || line.contains("срок PUT")
        || line.contains("до выхода хука")
}

/// Строка `hooks.log` о снимке сессии, не ушедшем в срок (M6 §8а): срок `PUT` или поток снимка,
/// не успевший к выходу хука, — снимок остаётся в очереди.
fn is_snapshot_late(line: &str) -> bool {
    line.contains("срок PUT")
        || line.contains("до выхода хука")
        || (line.contains("снимок") && line.contains("бюджет исчерпан"))
}

/// Строка `hooks.log` об удавшейся отправке снимка сессии (M6 §8а) — итог работы, не отказ.
fn is_snapshot_sent(line: &str) -> bool {
    line.contains("снимок") && line.contains(", отправлен")
}

/// Разброс времени пустого вызова `zam hook` в серии: p95 − минимум. На тихой машине — единицы
/// мс; на занятой — десятки и сотни: столько машина в этот момент прибавляет процессу, который
/// ничего не делает.
fn jitter(idle: &[Duration]) -> Duration {
    match idle.iter().min() {
        Some(min) => common::quantile(idle, 95).saturating_sub(*min),
        None => Duration::ZERO,
    }
}

/// Записи журнала Claude Code (как в `hooks.rs`): запрос человека, команда, правка файла. Без
/// `sessionId`: такая запись — своя для любой сессии (адаптер снимка пропускает только записи с
/// чужим id), один журнал служит всем сессиям серии.
fn cl_user(text: &str) -> String {
    json!({"type": "user", "isSidechain": false,
           "message": {"role": "user", "content": text}})
    .to_string()
}

fn cl_bash(id: &str, command: &str) -> String {
    json!({"type": "assistant", "isSidechain": false,
           "message": {"role": "assistant", "content": [
               {"type": "tool_use", "id": id, "name": "Bash", "input": {"command": command}}]}})
    .to_string()
}

fn cl_edit(id: &str, path: &Path) -> String {
    json!({"type": "assistant", "isSidechain": false,
           "message": {"role": "assistant", "content": [
               {"type": "tool_use", "id": id, "name": "Edit",
                "input": {"file_path": path, "old_string": "a", "new_string": "b"}}]}})
    .to_string()
}

fn append_lines(path: &Path, lines: &[String]) {
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    for l in lines {
        writeln!(f, "{l}").unwrap();
    }
}

/// Журнал агента не меньше `bytes` байт: запросы, команды и правки файлов проекта `repo`
/// вперемешку с выводом агента (строки без признаков предфильтра снимка — как у живого журнала
/// большая часть объёма).
fn big_transcript(path: &Path, repo: &Path, bytes: u64) {
    use std::io::Write as _;
    let filler = json!({"type": "progress", "data": "ж".repeat(2048)}).to_string();
    let mut f = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    let (mut n, mut i) = (0u64, 0usize);
    while n < bytes {
        let lines = [
            cl_user(&format!("Запрос {i}: почини пул соединений")),
            cl_bash(&format!("b{i}"), "cargo test -p pool"),
            cl_edit(&format!("e{i}"), &repo.join(format!("src/f{}.rs", i % 7))),
            filler.clone(),
        ];
        for l in lines {
            writeln!(f, "{l}").unwrap();
            n += l.len() as u64 + 1;
        }
        i += 1;
    }
    f.flush().unwrap();
}

/// Размер журнала агента в замерах снимка (M6, задача 17d): 50 МиБ.
const BIG_TRANSCRIPT: u64 = 50 * 1024 * 1024;

/// Удаляет очереди изменённых файлов всех сессий проекта (и забранные, но не возвращённые).
fn clear_changed(p: &Proj) {
    let cache = p.repo.join(".zeroaimemory/cache");
    for e in std::fs::read_dir(&cache).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with("changed-") || name.starts_with(".changed-") {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Очередь изменённых файлов сессии `changed-<сессия>.jsonl` вручную (имя — как у хуков:
/// очищенный id и 8 hex blake3).
fn put_changed(p: &Proj, session: &str, path: &str) {
    let cache = p.repo.join(".zeroaimemory/cache");
    std::fs::create_dir_all(&cache).unwrap();
    let part = format!(
        "{session}-{}",
        &blake3::hash(session.as_bytes()).to_hex()[..8]
    );
    std::fs::write(
        cache.join(format!("changed-{part}.jsonl")),
        format!("{}\n", json!({"path": path, "hash": "b".repeat(64)})),
    )
    .unwrap();
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
    // время — внутри процесса, лучший из повторов (вывод — первого вызова: подсказка — раз за
    // сессию)
    let (o, d) = b.hook_best(
        &b.url,
        "pre-tool",
        &b.write_ev("crlf-b-1", "deploy.sh"),
        fail_open_max("pre-tool"),
    );
    let c = context(&o, "PreToolUse");
    assert!(
        c.starts_with("ZeroAIMemory: для **/*.sh: Скрипты *.sh — только LF [gotcha·hyp]"),
        "{c}"
    );
    assert!(c.contains(&format!("i:{id}")), "{c}");
    assert!(d <= fail_open_max("pre-tool"), "pre-tool без сети: {d:?}");
    let (o, d) = b.hook_best(
        &b.url,
        "post-tool",
        &b.fail_ev(
            "crlf-b-1",
            "Exit code 126\n/bin/bash^M: bad interpreter: No such file or directory",
        ),
        fail_open_max("post-tool"),
    );
    let c = context(&o, "PostToolUseFailure");
    assert!(
        c.contains(&format!(
            "ZeroAIMemory: эта ошибка уже разбиралась: Скрипты *.sh — только LF [gotcha·hyp] \
             i:{id}"
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
    // M4.5: запись со ссылкой на файл — знание о файле для `post-tool` успеха (`Read`)
    let (code, v) = srv.call(
        0,
        "POST",
        &format!("/api/v1/spaces/{GA}/entries"),
        Some(
            &json!({"kind": "insight", "title": "Пул соединений течёт при панике",
                     "body": "Соединение не возвращается в пул, если обработчик паникует.",
                     "data": {"kind": "gotcha"}, "force": true,
                     "refs": {"files": [{"path": "src/pool.rs"}]}}),
        ),
        &[],
    );
    assert_eq!(code, 201, "{v}");
    // M5.7: 50 идей человека в решении — раздел `### Идеи` дайджеста и отдельный поиск идей
    // к `prompt` входят в те же бюджеты
    let topics = ["кэш", "деплой", "поиск", "логирование", "миграции"];
    for i in 0..50 {
        let (code, v) = srv.call(
            0,
            "POST",
            &format!("/api/v1/spaces/{GA}/entries"),
            Some(
                &json!({"kind": "idea", "title": format!("Идея {i}: улучшить {}", topics[i % 5]),
                         "body": "Замысел на будущее.", "data": {"origin": "human"}}),
            ),
            &[],
        );
        assert_eq!(code, 201, "{v}");
    }
    std::fs::create_dir_all(p.repo.join("src")).unwrap();
    std::fs::write(p.repo.join("src/pool.rs"), "pub fn pool() {}\n").unwrap();
    // M6 §12 (§17 п. 15): `checkpoint` меряется с влиянием диффа
    std::fs::write(
        p.repo.join(".zeroaimemory/config.toml"),
        "[hooks]\ncheckpoint_impact = true\n",
    )
    .unwrap();
    // кэш триггеров — до pre-tool
    p.session_start_until("bud-warm", |c| !c.is_empty());
    assert!(
        p.repo.join(".zeroaimemory/cache/file-refs.json").is_file(),
        "session-start не записал file-refs.json; hooks.log:\n{}",
        p.log()
    );
    // M6 §7.1: замер `session-start` — с записью привязки `zam mcp` к сессии
    let bound = std::fs::read_dir(p.repo.join(".zeroaimemory/cache"))
        .unwrap()
        .flatten()
        .any(|e| e.file_name().to_string_lossy().starts_with("agent-"));
    assert!(bound, "session-start не записал agent-<pid>.json");
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
    // M4.5: `post-tool` успеха — чтение файла со знанием (своя сессия: строка о файле — раз за
    // сессию) и без него, правка, команда; детерминирован (локальный кэш, без сети)
    runs.push((
        "post-tool-ok",
        (0..40)
            .map(|i| {
                let s = format!("bud-ok-{i}");
                let (tool, input, want) = match i % 4 {
                    0 => ("Read", json!({"file_path": p.repo.join("src/pool.rs")}), true),
                    1 => ("Read", json!({"file_path": p.repo.join("README.md")}), false),
                    2 => (
                        "Edit",
                        json!({"file_path": p.repo.join("a.txt"), "old_string": "a", "new_string": "b"}),
                        false,
                    ),
                    _ => ("Bash", json!({"command": "cargo build"}), false),
                };
                let ev = p.ev(
                    "PostToolUse",
                    &s,
                    json!({"tool_name": tool, "tool_use_id": format!("u{i}"), "tool_input": input,
                           "tool_response": {"success": true}}),
                );
                (ev, Some(want))
            })
            .collect(),
    ));
    // M4.5: `subagent-start` — пакет под задачу после `pre-tool` на `Agent` (очередь запусков);
    // задание — текст запроса prompt набора подсказок, ответ обязателен (пакет или мини-дайджест;
    // замолчавшие по сроку из этой проверки исключены, они считаются в тайм-аутах)
    let prompts = inputs("prompt");
    runs.push((
        "subagent-start",
        (0..20)
            .map(|i| {
                let ev = p.ev(
                    "SubagentStart",
                    &format!("bud-sub-{i}"),
                    json!({"agent_id": format!("a{i}"), "agent_type": "Explore"}),
                );
                (ev, Some(true))
            })
            .collect(),
    ));
    // M6 §12 (ревью задачи 12, I2): `session-start` контрольной группы — метка до `/recall` не
    // выводит хук за бюджет; сессии — те, что гарантированно в группе при p = 0,5
    runs.push((
        "session-start-holdout",
        (0..)
            .map(|i| format!("bud-ho-{i}"))
            .filter(|s| common::holdout_pick(s, 0.5))
            .take(40)
            .map(|s| (p.start_ev(&s), Some(false)))
            .collect(),
    ));
    // M6 §8а (задача 17d): снимок сессии из журнала агента 50 МиБ. Серии идут последними: их
    // `snapst-*` иначе стали бы кандидатами догоняющего снимка у прежних серий `session-start`.
    // Первые `SNAP_WARM` событий серии — прогрев на зарегистрированных сессиях (не в замер).
    let big = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("cli-m4-budgets-transcripts");
    let _ = std::fs::remove_dir_all(&big);
    std::fs::create_dir_all(&big).unwrap();
    let (t_stop, t_catch) = (big.join("stop.jsonl"), big.join("catchup.jsonl"));
    big_transcript(&t_stop, &p.repo, BIG_TRANSCRIPT);
    std::fs::copy(&t_stop, &t_catch).unwrap();
    const SNAP_WARM: usize = 3;
    // `checkpoint` (`Stop`) с напоминанием, влиянием диффа и первым снимком сессии: чтение хвоста
    // журнала, разбор, маскировка, очередь, `PUT` — в бюджете 100 мс
    runs.push((
        "checkpoint-snap",
        (0..40 + SNAP_WARM)
            .map(|i| {
                let mut ev = p.ev(
                    "Stop",
                    &format!("bud-cs-{i}"),
                    json!({"stop_hook_active": false}),
                );
                ev["transcript_path"] = json!(t_stop);
                (ev, Some(true))
            })
            .collect(),
    ));
    // `session-start` (`startup`) с догоняющим снимком оборванной сессии: снимок и `PUT` — до
    // `/recall`, всё вместе — в бюджете 300 мс
    runs.push((
        "session-start-catchup",
        (0..40 + SNAP_WARM)
            .map(|i| (p.start_ev(&format!("bud-cu-{i}")), Some(true)))
            .collect(),
    ));
    let mut report = Vec::new();
    let mut fails = Vec::new();
    for (kind, evs) in &runs {
        let snap_run = matches!(*kind, "checkpoint-snap" | "session-start-catchup");
        std::fs::write(
            p.repo.join(".zeroaimemory/config.toml"),
            if *kind == "session-start-holdout" {
                "[hooks]\ncheckpoint_impact = true\nholdout = 0.5\n"
            } else {
                "[hooks]\ncheckpoint_impact = true\n"
            },
        )
        .unwrap();
        if matches!(*kind, "checkpoint" | "checkpoint-snap") {
            // правка в сессии — иначе checkpoint молчит без запроса к серверу; правка файла со
            // знанием (`src/pool.rs`) — вход влияния диффа (M6 §12)
            for (ev, _) in evs.iter() {
                let s = ev["session_id"].as_str().unwrap().to_string();
                if snap_run {
                    // сессия на сервере: `PUT` снимка незарегистрированной сессии — 404
                    p.hook("session-start", &p.start_ev(&s));
                }
                p.hook("pre-tool", &p.write_ev(&s, "a.txt"));
                let edit = p.ev(
                    "PostToolUse",
                    &s,
                    json!({"tool_name": "Edit", "tool_use_id": "u1",
                           "tool_input": {"file_path": p.repo.join("src/pool.rs"),
                                          "old_string": "a", "new_string": "b"},
                           "tool_response": {"success": true}}),
                );
                p.hook("post-tool", &edit);
            }
        }
        if matches!(*kind, "pre-tool" | "post-tool-ok") {
            // файлы сессии (журнал событий, маркеры) — до замера: у живой сессии их создаёт первый
            // же вызов инструмента, а каждый следующий только дописывает. Без этого каждый замер
            // шёл бы на новой сессии и мерил создание файлов — на HDD оно стоит до сотен мс
            // (замер 17d, release: 2 вызова из 18 — 108 и 174 мс при 3 мс у остальных)
            for (ev, _) in evs.iter() {
                let s = ev["session_id"].as_str().unwrap();
                let read = p.ev(
                    "PreToolUse",
                    s,
                    json!({"tool_name": "Read", "tool_use_id": "u0",
                           "tool_input": {"file_path": p.repo.join("LICENSE")}}),
                );
                p.hook("pre-tool", &read);
            }
        }
        if *kind == "subagent-start" {
            // запуск субагента в очереди сессии — до замера
            for (i, (ev, _)) in evs.iter().enumerate() {
                let task = prompts[i % prompts.len()]["input"].as_str().unwrap();
                let spawn = p.ev(
                    "PreToolUse",
                    ev["session_id"].as_str().unwrap(),
                    json!({"tool_name": "Agent", "tool_use_id": "u1",
                           "tool_input": {"description": "d", "prompt": task,
                                          "subagent_type": "Explore"}}),
                );
                p.hook("pre-tool", &spawn);
            }
        }
        // команда хука: `post-tool-ok` — тот же `post-tool` с событием успеха
        let cmd = match *kind {
            "post-tool-ok" => "post-tool",
            "session-start-holdout" | "session-start-catchup" => "session-start",
            "checkpoint-snap" => "checkpoint",
            k => k,
        };
        // прогрев вида (не в замер): после загрузки корпуса первые запросы отладочного сервера
        // читают холодные страницы SQLite с HDD — бюджет относится к рабочему серверу
        if !snap_run {
            for (ev, _) in evs.iter().take(3) {
                let mut w = ev.clone();
                w["session_id"] = json!(format!("warm-{}", ev["session_id"].as_str().unwrap()));
                p.hook_at(&p.url, cmd, &w);
            }
        }
        let mut times: Vec<common::HookTimes> = Vec::new();
        let mut idle = Vec::new();
        let (mut timeouts, mut spoke, mut impact, mut sent, mut put_late) = (0, 0, 0, 0, 0);
        for (i, (ev, want)) in evs.iter().enumerate() {
            if *kind == "session-start-catchup" {
                // оборванная сессия: её снимок на `Stop`, затем журнал вырос (работа шла дальше,
                // а `SessionEnd` не пришёл) — следующий старт снимает её догоняющим снимком
                let a = format!("bud-ca-{i}");
                p.hook("session-start", &p.start_ev(&a));
                let mut stop = p.ev("Stop", &a, json!({"stop_hook_active": false}));
                stop["transcript_path"] = json!(t_catch);
                p.hook("checkpoint", &stop);
                append_lines(
                    &t_catch,
                    &[
                        cl_user(&format!("Продолжение {i}: добавь тест на пул")),
                        cl_edit(&format!("c{i}"), &p.repo.join("src/pool.rs")),
                    ],
                );
            }
            let measured = !snap_run || i >= SNAP_WARM;
            // пустой вызов рядом с каждым замером: что машина сейчас делает с процессом
            let empty_call = p.idle(cmd);
            let before = p.log().lines().count();
            let (o, t) = p.hook_times(&p.url, cmd, ev);
            if !measured {
                continue;
            }
            idle.push(empty_call);
            let said = !ok(&o).trim().is_empty();
            spoke += usize::from(said);
            impact += usize::from(out(&o).contains("правка затрагивает"));
            times.push(t);
            let lines: Vec<String> = p.log().lines().skip(before).map(String::from).collect();
            // снимок, не ушедший в срок, — не отказ хука: он в очереди (§8а), вывод хука не
            // задержан; считается отдельно. Срок остальных запросов — тайм-аут, как прежде
            let snap_late = lines.iter().any(|l| is_snapshot_late(l));
            put_late += usize::from(snap_late);
            let late = lines.iter().any(|l| is_timeout(l) && !is_snapshot_late(l));
            timeouts += usize::from(late);
            sent += usize::from(lines.iter().any(|l| is_snapshot_sent(l)));
            // любая другая строка журнала — отказ, а не замер (I-1)
            for l in lines
                .iter()
                .filter(|l| !is_timeout(l) && !is_snapshot_sent(l))
            {
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
        let outer: Vec<Duration> = times.iter().map(|t| t.outer).collect();
        let inner: Vec<Duration> = times.iter().filter_map(|t| t.inner).collect();
        let n = outer.len();
        let budget = Duration::from_millis(budget_ms(kind));
        let noise = jitter(&idle);
        let (mid, q) = (common::quantile(&outer, 50), common::quantile(&outer, 95));
        report.push(format!(
            "{kind} (бюджет {} мс): процесс {}; внутри {}; пустой вызов {}; ответов={spoke} \
             тайм-аутов={timeouts}{}{}",
            budget_ms(kind),
            common::stats(&outer),
            common::stats(&inner),
            common::stats(&idle),
            if matches!(*kind, "checkpoint" | "checkpoint-snap") {
                format!(" со строками влияния={impact}")
            } else {
                String::new()
            },
            if snap_run {
                format!(" снимков отправлено={sent} в очереди по сроку={put_late}")
            } else {
                String::new()
            }
        ));
        // влияние диффа (M6 §12) уложилось в свой срок 60 мс хотя бы раз: иначе замер
        // `checkpoint` шёл бы без `POST /code/impact` (молчание по сроку — не отказ)
        if matches!(*kind, "checkpoint" | "checkpoint-snap") && impact == 0 {
            fails.push(format!("{kind}: ни одной строки влияния диффа"));
        }
        if cfg!(debug_assertions) {
            // отладочная сборка (CI, общие раннеры): запас ×2 к времени процесса
            let limit = budget * CI_MARGIN as u32;
            if q > limit {
                fails.push(format!("{kind}: p95 {q:?} > {limit:?}"));
            }
        } else {
            // release (приёмка, задача 17d): бюджет §10.3 как он есть, по времени процесса
            // целиком — его и ждёт агент, и только оно видит задержку, общую для любого вызова
            // `zam hook` (запуск, выход). Медиана — устойчива к остановкам диска; p95 — с
            // поправкой на то, что машина в этой же серии прибавила пустому вызову
            if mid > budget {
                fails.push(format!(
                    "{kind}: медиана времени процесса {mid:?} > бюджета {budget:?}"
                ));
            }
            if q > budget + noise {
                fails.push(format!(
                    "{kind}: p95 времени процесса {q:?} > бюджета {budget:?} + разброс пустого \
                     вызова {noise:?}"
                ));
            }
        }
        // сроки снимка (`hooks.snapshot_ms`, срок `PUT`) — бюджеты release: отладочная сборка на
        // общем раннере в них не обязана укладываться, там серии снимка проверяют только отказы
        let strict = !snap_run || !cfg!(debug_assertions);
        if strict && timeouts * 20 > n {
            fails.push(format!("{kind}: тайм-аутов {timeouts} из {n} (> 5 %)"));
        }
        // снимок состоялся: каждый вызов либо отправил снимок, либо оставил его в очереди со
        // строкой причины — иначе замер шёл бы без чтения журнала и `PUT` (молчаливый пропуск
        // снимка — отказ); в срок уходит подавляющая часть (не меньше 80 %: срок `PUT` и срок
        // снимка рассчитаны на эту машину, а не «иногда успевает»)
        if snap_run && strict && sent + put_late < n {
            fails.push(format!(
                "{kind}: снимок отправлен в {sent} и оставлен в очереди в {put_late} из {n}"
            ));
        }
        if snap_run && strict && sent * 5 < n * 4 {
            fails.push(format!(
                "{kind}: снимок отправлен в срок в {sent} из {n} (< 80 %)"
            ));
        }
        // контрольная группа молчит по построению; её метка — на сервере у каждой сессии
        if *kind == "session-start-holdout" {
            for (ev, _) in evs {
                let id = ev["session_id"].as_str().unwrap();
                let (code, v) = srv.call(0, "GET", &format!("/api/v1/sessions/{id}"), None, &[]);
                if code != 200 || v["holdout"] != true {
                    fails.push(format!("{kind} {id}: нет метки holdout ({code}: {v})"));
                }
            }
        } else if spoke == 0 {
            fails.push(format!("{kind}: ни одного ответа из {n}"));
        }
    }
    let _ = std::fs::remove_dir_all(&big);
    eprintln!("ZAM_TIMING m4 hook_budgets:\n  {}", report.join("\n  "));
    assert!(
        fails.is_empty(),
        "{}\n{}\nhooks.log:\n{}",
        fails.join("\n"),
        report.join("\n"),
        p.log()
    );
}

/// Задача 17d (пункт d): хук, запросы которого не состоялись — сервер принял соединение и
/// молчит. Срок HTTP — бюджет минус резерв (`HTTP_RESERVE_MS`), но ожидание сокета возвращается
/// с шагом таймера ОС (Windows — до 15,6 мс), а после него хук ещё пишет строки `hooks.log` и
/// возвращает очередь изменённых файлов. Серия по каждому сетевому хуку (очередь изменённых
/// есть у каждого вызова): время выхода — в бюджете.
///
/// Что ловит: рост «хвоста после срока» (лишние файловые операции после возврата запроса,
/// второй срок сверх первого, ожидание потока) и уменьшение резерва. Release (приёмка): медиана
/// времени процесса целиком не больше бюджета, p95 — бюджета и разброса пустого вызова. Отладочная
/// сборка: время внутри процесса, медиана не больше бюджета + запаса отладочной сборки, p95 —
/// полутора бюджетов (как `max_inner` в `hooks.rs`) и разброса пустого вызова.
#[test]
fn hook_budgets_unanswered() {
    let _g = serial();
    let hang = hanging_server();
    let p = Proj::new("budgets-hang", &hang, "zam_x", GA, "bench-box");
    let mut report = Vec::new();
    let mut fails = Vec::new();
    const WARM: usize = 3;
    for kind in ["session-start", "prompt", "post-tool", "checkpoint"] {
        let (mut times, mut idle) = (Vec::new(), Vec::new());
        for i in 0..30 + WARM {
            let s = format!("un-{kind}-{i}");
            // очередь изменённых файлов сессии: её отправка и возврат — в пути хука. Одна на
            // вызов: `session-start` отправляет очереди всех сессий проекта, и без уборки они
            // копились бы от вызова к вызову (масштаб по числу очередей — ниже, отдельно)
            clear_changed(&p);
            put_changed(&p, &s, &format!("src/f{i}.rs"));
            let ev = match kind {
                "session-start" => p.start_ev(&s),
                "prompt" => p.ev(
                    "UserPromptSubmit",
                    &s,
                    json!({"prompt": "почему течёт пул соединений"}),
                ),
                "post-tool" => p.fail_ev(&s, "error[E0308]: mismatched types"),
                _ => {
                    // правка в сессии: `checkpoint` идёт на сервер за `GET /sessions/{id}`
                    empty(&p.hook_at(&hang, "pre-tool", &p.write_ev(&s, "a.txt")).0);
                    p.ev("Stop", &s, json!({"stop_hook_active": false}))
                }
            };
            let empty_call = p.idle(kind);
            let before = p.log().lines().count();
            let (o, t) = p.hook_times(&hang, kind, &ev);
            empty(&o);
            if i < WARM {
                continue;
            }
            // запросы действительно не состоялись: строка причины в журнале есть
            if !p.log().lines().skip(before).any(is_timeout) {
                fails.push(format!(
                    "{kind} {s}: нет строки о сроке; hooks.log: {:?}",
                    p.log().lines().skip(before).collect::<Vec<_>>()
                ));
            }
            times.push(t);
            idle.push(empty_call);
        }
        let outer: Vec<Duration> = times.iter().map(|t| t.outer).collect();
        let inner: Vec<Duration> = times.iter().filter_map(|t| t.inner).collect();
        let tail: Vec<Duration> = times.iter().filter_map(|t| t.launch()).collect();
        let budget = Duration::from_millis(budget_ms(kind));
        let noise = jitter(&idle);
        report.push(format!(
            "{kind} (бюджет {} мс): процесс {}; внутри {}; запуск и выход {}; пустой вызов {}",
            budget_ms(kind),
            common::stats(&outer),
            common::stats(&inner),
            common::stats(&tail),
            common::stats(&idle),
        ));
        if cfg!(debug_assertions) {
            let (mid, q) = (common::quantile(&inner, 50), common::quantile(&inner, 95));
            if mid > budget + DEBUG_EXIT_SLACK {
                fails.push(format!(
                    "{kind}: медиана внутри процесса {mid:?} > бюджета {budget:?} + \
                     {DEBUG_EXIT_SLACK:?}"
                ));
            }
            if q > fail_open_max(kind) + noise {
                fails.push(format!(
                    "{kind}: p95 внутри процесса {q:?} > {:?} + разброс пустого вызова {noise:?}",
                    fail_open_max(kind)
                ));
            }
        } else {
            let (mid, q) = (common::quantile(&outer, 50), common::quantile(&outer, 95));
            if mid > budget {
                fails.push(format!(
                    "{kind}: медиана времени процесса {mid:?} > бюджета {budget:?}"
                ));
            }
            if q > budget + noise {
                fails.push(format!(
                    "{kind}: p95 времени процесса {q:?} > бюджета {budget:?} + разброс пустого \
                     вызова {noise:?}"
                ));
            }
        }
    }
    // масштаб: сервер недоступен давно, очереди изменённых файлов есть у K сессий —
    // `session-start` забирает и возвращает каждую. Предел — тот же бюджет: число очередей не
    // должно выводить хук за него (`FLUSH_QUEUES_MAX`, возврат забранной очереди одним rename)
    for k in [8usize, 32] {
        let mut times = Vec::new();
        for i in 0..12 + WARM {
            clear_changed(&p);
            for q in 0..k {
                put_changed(&p, &format!("un-many-{k}-{q}"), &format!("src/q{q}.rs"));
            }
            let ev = p.start_ev(&format!("un-many-{k}-s{i}"));
            let (o, t) = p.hook_times(&hang, "session-start", &ev);
            empty(&o);
            if i >= WARM {
                times.push(t);
            }
            // ни одна очередь не потеряна
            let left = std::fs::read_dir(p.repo.join(".zeroaimemory/cache"))
                .unwrap()
                .flatten()
                .filter(|e| {
                    let n = e.file_name().to_string_lossy().into_owned();
                    n.starts_with("changed-") && n.ends_with(".jsonl")
                })
                .count();
            if left != k {
                fails.push(format!(
                    "session-start, очередей {k}: после хука осталось {left}; hooks.log:\n{}",
                    p.log().lines().rev().take(6).collect::<Vec<_>>().join("\n")
                ));
            }
        }
        let outer: Vec<Duration> = times.iter().map(|t| t.outer).collect();
        let inner: Vec<Duration> = times.iter().filter_map(|t| t.inner).collect();
        report.push(format!(
            "session-start, очередей {k} (бюджет 300 мс): процесс {}; внутри {}",
            common::stats(&outer),
            common::stats(&inner),
        ));
        let series = if cfg!(debug_assertions) {
            &inner
        } else {
            &outer
        };
        let mid = common::quantile(series, 50);
        let max = if cfg!(debug_assertions) {
            fail_open_max("session-start")
        } else {
            Duration::from_millis(budget_ms("session-start"))
        };
        if mid > max {
            fails.push(format!(
                "session-start, очередей {k}: медиана {mid:?} > {max:?} — время выхода растёт с \
                 числом очередей"
            ));
        }
    }
    clear_changed(&p);
    eprintln!(
        "ZAM_TIMING m4 hook_budgets_unanswered:\n  {}",
        report.join("\n  ")
    );
    assert!(
        fails.is_empty(),
        "{}\n{}",
        fails.join("\n"),
        report.join("\n")
    );
}

/// Решение замера против внешнего сервера ([`hook_budgets_remote`]).
const GR: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9ec1";

/// Задача 17d (пункт c): те же хуки со снимком сессии против **внешнего** сервера — по LAN, на
/// медленном диске, за TLS-прокси. Запуск вручную:
///
/// ```text
/// ZAM_HOOK_BUDGETS_URL=http://192.168.0.210:7711 ZAM_HOOK_BUDGETS_TOKEN=zam_… \
///   cargo test -p zam-cli --release --test m4_acceptance hook_budgets_remote -- --nocapture
/// ```
///
/// Без переменных — пропуск. Серии (`ZAM_HOOK_BUDGETS_N` вызовов, по умолчанию 40, журнал
/// агента 50 МиБ): `session-start`; `checkpoint` (`Stop` с напоминанием) без снимка и со
/// снимком; `SessionEnd` со снимком (`PATCH {ended}` и `PUT`, без вывода); `session-start` с
/// догоняющим снимком; и сам `PUT /sessions/{id}/snapshot` снимка с журнала (новое соединение
/// на запрос, как у хука). Печатает сводки (`ZAM_TIMING`) и исходы `PUT` по `hooks.log`
/// («отправлен», «срок PUT», прочее); падает только на отказах, не на времени — бюджет
/// на чужой среде не критерий, а предмет замера (решение о сроке `PUT`, журнал M6).
#[test]
fn hook_budgets_remote() {
    let (Ok(url), Ok(token)) = (
        std::env::var("ZAM_HOOK_BUDGETS_URL"),
        std::env::var("ZAM_HOOK_BUDGETS_TOKEN"),
    ) else {
        eprintln!("SKIP hook_budgets_remote: нет ZAM_HOOK_BUDGETS_URL и ZAM_HOOK_BUDGETS_TOKEN");
        return;
    };
    let _g = serial();
    let n: usize = std::env::var("ZAM_HOOK_BUDGETS_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(40);
    const WARM: usize = 3;
    let auth = format!("Bearer {token}");
    let h = [("Authorization", auth.as_str())];
    let (code, v) = rest(
        &url,
        &h,
        "POST",
        "/api/v1/solutions",
        Some(&json!({"agent_solution_id": GR, "name": "Замер хуков"})),
    );
    assert!(
        matches!(code, 200 | 201 | 409),
        "регистрация решения: {code} {v}"
    );
    let p = Proj::new("budgets-remote", &url, &token, GR, "bench-box");
    // свой прогон — свои сессии: сервер живёт между запусками
    let run = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    std::fs::create_dir_all(p.repo.join("src")).unwrap();
    std::fs::write(p.repo.join("src/pool.rs"), "pub fn pool() {}\n").unwrap();
    let big = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("cli-m4-remote-transcripts");
    let _ = std::fs::remove_dir_all(&big);
    std::fs::create_dir_all(&big).unwrap();
    let (t_stop, t_catch) = (big.join("stop.jsonl"), big.join("catchup.jsonl"));
    big_transcript(&t_stop, &p.repo, BIG_TRANSCRIPT);
    std::fs::copy(&t_stop, &t_catch).unwrap();
    let with = |mut ev: Value, t: &Path| {
        ev["transcript_path"] = json!(t);
        ev
    };
    let mut report = Vec::new();
    let mut fails = Vec::new();
    for kind in [
        "session-start",
        "checkpoint",
        "checkpoint-snap",
        "checkpoint-end",
        "session-start-catchup",
    ] {
        let cmd = if kind.starts_with("checkpoint") {
            "checkpoint"
        } else {
            "session-start"
        };
        let (mut times, mut idle) = (Vec::new(), Vec::new());
        let (mut sent, mut put_late, mut late, mut spoke) = (0, 0, 0, 0);
        let mut other: Vec<String> = Vec::new();
        for i in 0..n + WARM {
            let s = format!("rem-{run}-{kind}-{i}");
            let ev = match kind {
                "session-start" => p.start_ev(&s),
                "session-start-catchup" => {
                    let a = format!("rem-{run}-ca-{i}");
                    p.hook("session-start", &p.start_ev(&a));
                    p.hook(
                        "checkpoint",
                        &with(
                            p.ev("Stop", &a, json!({"stop_hook_active": false})),
                            &t_catch,
                        ),
                    );
                    append_lines(
                        &t_catch,
                        &[
                            cl_user(&format!("Продолжение {i}: добавь тест на пул")),
                            cl_edit(&format!("c{i}"), &p.repo.join("src/pool.rs")),
                        ],
                    );
                    p.start_ev(&s)
                }
                _ => {
                    // сессия на сервере и правка в ней: `Stop` идёт за `GET /sessions/{id}`
                    p.hook("session-start", &p.start_ev(&s));
                    p.hook("pre-tool", &p.write_ev(&s, "a.txt"));
                    match kind {
                        "checkpoint" => p.ev("Stop", &s, json!({"stop_hook_active": false})),
                        "checkpoint-snap" => with(
                            p.ev("Stop", &s, json!({"stop_hook_active": false})),
                            &t_stop,
                        ),
                        _ => with(p.ev("SessionEnd", &s, json!({"reason": "other"})), &t_stop),
                    }
                }
            };
            let empty_call = p.idle(cmd);
            let before = p.log().lines().count();
            let (o, t) = p.hook_times(&url, cmd, &ev);
            if i < WARM {
                continue;
            }
            spoke += usize::from(!ok(&o).trim().is_empty());
            times.push(t);
            idle.push(empty_call);
            let lines: Vec<String> = p.log().lines().skip(before).map(String::from).collect();
            sent += usize::from(lines.iter().any(|l| is_snapshot_sent(l)));
            put_late += usize::from(lines.iter().any(|l| l.contains("срок PUT")));
            late += usize::from(lines.iter().any(|l| is_timeout(l)));
            other.extend(
                lines
                    .into_iter()
                    .filter(|l| !is_timeout(l) && !is_snapshot_sent(l)),
            );
        }
        let outer: Vec<Duration> = times.iter().map(|t| t.outer).collect();
        let inner: Vec<Duration> = times.iter().filter_map(|t| t.inner).collect();
        report.push(format!(
            "{kind} (бюджет {} мс): процесс {}; внутри {}; пустой вызов {}; ответов={spoke} \
             вызовов со строкой срока={late} снимков отправлено={sent} «срок PUT»={put_late}",
            if cmd == "checkpoint" { 100 } else { 300 },
            common::stats(&outer),
            common::stats(&inner),
            common::stats(&idle),
        ));
        for l in other.iter().take(5) {
            fails.push(format!("{kind}: ошибка хука: {l}"));
        }
    }
    // сам `PUT` снимка: тело — снимок, принятый сервером от хука; каждый запрос — новое
    // соединение (как у процесса хука); `taken_at` растёт, иначе сервер не пишет («stale_ignored»)
    let probe = format!("rem-{run}-put");
    p.hook("session-start", &p.start_ev(&probe));
    p.hook(
        "checkpoint",
        &with(
            p.ev("Stop", &probe, json!({"stop_hook_active": true})),
            &t_stop,
        ),
    );
    let path = format!("/api/v1/sessions/{probe}/snapshot");
    let (code, snap) = rest(&url, &h, "GET", &path, None);
    if code == 200 {
        let hs = [
            ("Authorization", auth.as_str()),
            ("X-Zam-Session", probe.as_str()),
        ];
        let mut puts = Vec::new();
        for i in 0..n + WARM {
            let body = json!({
                "taken_at": (zam_store::time::now() + Duration::from_millis(i as u64)).to_string(),
                "trigger": snap["trigger"], "agent": snap["agent"], "status": snap["status"],
                "skipped": snap["skipped"], "data": snap["data"],
            });
            let t0 = Instant::now();
            let (code, _, _) = rest_raw(&url, &hs, "PUT", &path, Some(&body));
            let d = t0.elapsed();
            if !matches!(code, 200 | 204) {
                fails.push(format!("PUT снимка: HTTP {code}"));
                break;
            }
            if i >= WARM {
                puts.push(d);
            }
        }
        report.push(format!(
            "PUT снимка ({} байт, срок клиента 60 мс): {}",
            snap["bytes"],
            common::stats(&puts)
        ));
    } else {
        fails.push(format!("снимок пробной сессии не дошёл: GET {code} {snap}"));
    }
    let _ = std::fs::remove_dir_all(&big);
    eprintln!(
        "ZAM_TIMING m4 hook_budgets_remote {url}:\n  {}",
        report.join("\n  ")
    );
    assert!(
        fails.is_empty(),
        "{}\n{}",
        fails.join("\n"),
        report.join("\n")
    );
}

/// Запас отладочной сборки к бюджету у медианы времени выхода хука с несостоявшимися запросами
/// ([`hook_budgets_unanswered`]): отладочный exe после срока запроса пишет строки `hooks.log` и
/// возвращает очередь медленнее release (замер 17b-http: 283–307 мс при бюджете 300).
const DEBUG_EXIT_SLACK: Duration = Duration::from_millis(30);

/// Раздел снимка сессии в тексте дайджеста (до следующего раздела `###`).
fn snapshot_section(digest: &str) -> Option<&str> {
    let at = digest.find("### Последнее известное состояние")?;
    let rest = &digest[at..];
    let end = rest[3..].find("\n### ").map_or(rest.len(), |i| i + 3);
    Some(rest[..end].trim_end())
}

/// Решения сценариев снимка (у каждого сценария своё: снимок оборванной сессии показывается всем
/// следующим стартам решения).
const GS: [&str; 3] = [
    "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9eb1",
    "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9eb2",
    "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9eb3",
];

/// Задача 17d (пункт b; решение владельца 2026-11-10, М11): раздел «Последнее известное
/// состояние» в дайджесте следующей сессии — типовые сценарии через настоящие хуки и сервер:
///
/// - **вторая сессия после обычной** (`SessionEnd`) — раздела нет (0 токенов);
/// - **после `/clear`** (`SessionEnd reason=clear`, затем `SessionStart source=clear`) — нет;
/// - **после обрыва** (нет `SessionEnd`, журнал вырос после последнего снимка, сессия молчит
///   дольше 10 мин) — раздел есть: первый старт снимает оборванную сессию догоняющим снимком,
///   раздел — не больше `recall.snapshot_tokens` (400).
///
/// «Молчит дольше 10 мин» — часами сервера (`freeze_now`, фича `test-hooks` хранилища): сервер
/// идёт в процессе теста. Время стартов второй сессии с бюджетом продукта печатается
/// (`ZAM_TIMING`); критерий времени — у `hook_budgets`.
#[test]
fn hook_budgets_snapshot_scenarios() {
    let _g = serial();
    let srv = Srv::start("snap-scen", &["Анна"], |_| {});
    let mut report = Vec::new();
    for (k, name) in ["обычный конец", "/clear", "обрыв"].into_iter().enumerate() {
        assert_eq!(srv.register(0, GS[k], name), 201);
        let p = Proj::new(
            &format!("snap-scen-{k}"),
            &srv.url,
            &srv.users[0].token,
            GS[k],
            "box",
        );
        // запись в решении: дайджест непуст и без раздела снимка
        srv.insight(
            0,
            GS[k],
            "Пул соединений течёт при панике",
            "gotcha",
            json!([]),
        );
        std::fs::create_dir_all(p.repo.join("src")).unwrap();
        std::fs::write(p.repo.join("src/pool.rs"), "pub fn pool() {}\n").unwrap();
        let t = p.repo.parent().unwrap().join("agent.jsonl");
        append_lines(
            &t,
            &[
                cl_user("Почини утечку соединений в пуле при панике обработчика"),
                cl_edit("e1", &p.repo.join("src/pool.rs")),
                cl_bash("b1", "cargo test -p pool"),
            ],
        );
        let with_t = |mut ev: Value| {
            ev["transcript_path"] = json!(t);
            ev
        };
        // первая сессия: старт, запрос, правка, `Stop` со снимком
        let a = format!("scen-a-{k}");
        let d0 = context(
            &p.hook(
                "session-start",
                &with_t(p.ev("SessionStart", &a, json!({"source": "startup"}))),
            ),
            "SessionStart",
        );
        assert!(snapshot_section(&d0).is_none(), "{d0}");
        p.hook(
            "prompt",
            &with_t(p.ev(
                "UserPromptSubmit",
                &a,
                json!({"prompt": "Почини утечку соединений в пуле"}),
            )),
        );
        p.hook("pre-tool", &with_t(p.write_ev(&a, "src/pool.rs")));
        p.hook(
            "checkpoint",
            &with_t(p.ev("Stop", &a, json!({"stop_hook_active": true}))),
        );
        let (code, v) = srv.call(
            0,
            "GET",
            &format!("/api/v1/sessions/{a}/snapshot"),
            None,
            &[],
        );
        assert_eq!(
            code,
            200,
            "{name}: снимок `Stop` не дошёл: {v}\n{}",
            p.log()
        );
        let source = match k {
            0 => {
                p.hook(
                    "checkpoint",
                    &with_t(p.ev("SessionEnd", &a, json!({"reason": "prompt_input_exit"}))),
                );
                "startup"
            }
            1 => {
                p.hook(
                    "checkpoint",
                    &with_t(p.ev("SessionEnd", &a, json!({"reason": "clear"}))),
                );
                "clear"
            }
            _ => {
                // обрыв: работа шла и после последнего снимка, конца сессии нет
                append_lines(
                    &t,
                    &[
                        cl_user("Теперь добавь тест на возврат соединения"),
                        cl_edit("e2", &p.repo.join("src/pool.rs")),
                    ],
                );
                "startup"
            }
        };
        if k < 2 {
            let (code, v) = srv.call(0, "GET", &format!("/api/v1/sessions/{a}"), None, &[]);
            assert_eq!(code, 200, "{v}");
            assert!(
                v["ended_at"].is_string(),
                "{name}: нет ended_at: {v}\n{}",
                p.log()
            );
        }
        // первая сессия молчит 11 минут (часы сервера)
        let _clock =
            zam_store::time::freeze_now(zam_store::time::now() + Duration::from_secs(11 * 60));
        // содержимое — со щедрым бюджетом
        let b = format!("scen-b-{k}");
        let d1 = context(
            &p.hook(
                "session-start",
                &p.ev("SessionStart", &b, json!({"source": source})),
            ),
            "SessionStart",
        );
        let section = snapshot_section(&d1);
        let tokens = section.map_or(0, zam_store::count_tokens);
        if k < 2 {
            assert!(
                section.is_none(),
                "{name}: раздел снимка после завершённой сессии:\n{d1}"
            );
        } else {
            let sec = section.unwrap_or_else(|| {
                panic!("{name}: нет раздела снимка:\n{d1}\nhooks.log:\n{}", p.log())
            });
            assert!(sec.contains("src/pool.rs"), "{sec}");
            // догоняющий снимок снял работу после последнего `Stop`
            assert!(sec.contains("добавь тест на возврат соединения"), "{sec}");
            assert!(
                p.log().contains("снимок catch_up:"),
                "нет догоняющего снимка:\n{}",
                p.log()
            );
            assert!(
                tokens > 0 && tokens <= 400,
                "раздел {tokens} токенов:\n{sec}"
            );
        }
        // время старта следующих сессий с бюджетом продукта (справочно)
        let (mut times, mut shown) = (Vec::new(), 0);
        for i in 0..12 {
            let ev = p.ev(
                "SessionStart",
                &format!("scen-c-{k}-{i}"),
                json!({"source": source}),
            );
            let (o, tm) = p.hook_times(&p.url, "session-start", &ev);
            if i < 2 {
                continue;
            }
            times.push(tm.outer);
            shown += usize::from(out(&o).contains("### Последнее известное состояние"));
        }
        report.push(format!(
            "{name}: раздел снимка {tokens} токенов (оценка сервера), {} символов; дайджест {} \
             токенов; старт следующей сессии, процесс: {}; раздел в {shown} из {}",
            section.map_or(0, |s| s.chars().count()),
            zam_store::count_tokens(&d1),
            common::stats(&times),
            times.len(),
        ));
    }
    eprintln!(
        "ZAM_TIMING m4 snapshot_scenarios:\n  {}",
        report.join("\n  ")
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
///
/// Время (задача 17d, пункт i) — внутри процесса, лучший из трёх вызовов не больше полутора
/// бюджетов ([`fail_open_max`]): остановка диска повтором снимается, хук, ждущий мёртвый сервер
/// дольше срока, — нет. Запуск и выход процесса (вне бюджета) меряют `hooks.rs`
/// (`assert_launch`) и release `hook_budgets`.
#[test]
fn fail_open_when_server_down() {
    let _g = serial();
    let hang = hanging_server();
    let dead = dead_url();
    for (label, url) in [("down", dead.as_str()), ("hang", hang.as_str())] {
        let p = Proj::new(&format!("fail-{label}"), url, "zam_x", GA, "box");
        let s = format!("fo-{label}");
        // правка в сессии: checkpoint идёт на сервер (GET /sessions), а не молчит сразу
        let (o, d) = p.hook_best(
            url,
            "pre-tool",
            &p.write_ev(&s, "a.txt"),
            fail_open_max("pre-tool"),
        );
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
            let (o, d) = p.hook_best(url, kind, ev, fail_open_max(kind));
            empty(&o);
            assert!(
                d <= fail_open_max(kind),
                "{label} {kind}: {d:?} внутри процесса (лучший из трёх); hooks.log:
{}",
                p.log()
            );
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
