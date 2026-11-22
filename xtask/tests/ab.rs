//! Стенд агентного A/B `cargo xtask ab` (спецификация M6 §12, §17 п. 16; задача 12): отчёт по
//! плечам с интервалами на синтетической фикстуре — два транскрипта Claude Code и один Codex
//! (только поля, которые читает стенд, и посторонний текст-приманка), ответы заглушки сервера
//! (`/health`, `GET /sessions/{id}`, субагенты `GET /sessions?parent=` и обход списка, выдача без
//! сессии `GET /sessions/unattributed` — M7 §5.8 п. 4) и `runs.toml`. Из транскриптов читаются
//! только `usage` (и тип блоков — счёт вызовов инструментов); файлы не копируются и не меняются —
//! проверка тестом. Журналы агентов владельца тест не читает.

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

/// Текст-приманка в содержимом сообщений: в отчёт и вывод он попасть не может.
const BAIT: &str = "ПРИМАНКА-текст-сообщения-7f3a";

fn xtask(args: &[&str]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_xtask"));
    c.args(args);
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("ZAM_") {
            c.env_remove(k);
        }
    }
    c.output().expect("запуск xtask")
}

fn tmp(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join("ab").join(name);
    if d.exists() {
        fs::remove_dir_all(&d).unwrap();
    }
    fs::create_dir_all(&d).unwrap();
    d
}

/// Принятые заглушкой запросы: (путь, `Authorization`).
type Seen = Arc<Mutex<Vec<(String, String)>>>;

/// Что отвечает заглушка сервера ([`stub_server`]).
#[derive(Clone, Default)]
struct Server {
    /// `GET /sessions/{id}`: id → ответ (нет — 404).
    sessions: BTreeMap<String, Value>,
    /// Сессии субагентов (`parent_id`, `tokens`): `GET /sessions?parent=` и обход списка.
    subagents: Vec<Value>,
    /// `features` ответа `GET /health`; `None` — `/health` отвечает 404 (возможности неизвестны).
    features: Option<Vec<&'static str>>,
    /// Строки `GET /sessions/unattributed`; `None` — 404 (сервер до M6).
    unattributed: Option<Vec<Value>>,
}

impl Server {
    /// Сервер M7.2: признаки `sessions_parent` и `unattributed`, выдачи без сессии нет.
    fn current(sessions: BTreeMap<String, Value>) -> Server {
        Server {
            sessions,
            features: Some(vec!["m6", "unattributed", "sessions_parent"]),
            unattributed: Some(Vec::new()),
            ..Server::default()
        }
    }

    /// Ответ на запрос `path` (с параметрами).
    fn reply(&self, path: &str) -> (u16, String) {
        let (p, q) = path.split_once('?').unwrap_or((path, ""));
        let param = |k: &str| {
            q.split('&')
                .find_map(|kv| kv.strip_prefix(k)?.strip_prefix('='))
                .map(str::to_string)
        };
        let items = |v: Vec<Value>| (200, json!({ "items": v }).to_string());
        match p {
            "/api/v1/health" => match &self.features {
                Some(f) => (200, json!({"status": "ok", "features": f}).to_string()),
                None => (404, r#"{"status":404}"#.to_string()),
            },
            "/api/v1/sessions" => match param("parent") {
                Some(parent) if self.has("sessions_parent") => items(
                    self.subagents
                        .iter()
                        .filter(|s| s["parent_id"] == parent.as_str())
                        .cloned()
                        .collect(),
                ),
                // сервер без `parent`: `ListQuery` с `deny_unknown_fields`
                Some(_) => (
                    400,
                    r#"{"status":400,"detail":"unknown field `parent`"}"#.into(),
                ),
                None => items(
                    self.sessions
                        .values()
                        .chain(&self.subagents)
                        .cloned()
                        .collect(),
                ),
            },
            "/api/v1/sessions/unattributed" => match &self.unattributed {
                Some(v) => items(v.clone()),
                None => (404, r#"{"status":404,"detail":"нет"}"#.to_string()),
            },
            _ => {
                let id = p.strip_prefix("/api/v1/sessions/").unwrap_or("");
                match self.sessions.get(id) {
                    Some(v) => (200, v.to_string()),
                    None => (404, r#"{"status":404,"detail":"нет"}"#.to_string()),
                }
            }
        }
    }

    fn has(&self, f: &str) -> bool {
        self.features.as_ref().is_some_and(|x| x.contains(&f))
    }
}

/// Заглушка сервера M7.2 ([`Server::current`]) с сессиями `sessions`.
fn stub(sessions: BTreeMap<String, Value>) -> (String, Seen) {
    stub_server(Server::current(sessions))
}

/// Заглушка сервера `srv`; принятые пути и заголовки авторизации — в журнал.
fn stub_server(srv: Server) -> (String, Seen) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let seen: Seen = Arc::default();
    let s2 = seen.clone();
    std::thread::spawn(move || {
        for conn in l.incoming().flatten() {
            let mut r = BufReader::new(conn.try_clone().unwrap());
            let mut line = String::new();
            if r.read_line(&mut line).is_err() {
                continue;
            }
            let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
            let (mut auth, mut len) = (String::new(), 0usize);
            loop {
                let mut h = String::new();
                if r.read_line(&mut h).is_err() || h.trim().is_empty() {
                    break;
                }
                if let Some((k, v)) = h.split_once(':') {
                    if k.eq_ignore_ascii_case("authorization") {
                        auth = v.trim().to_string();
                    }
                    if k.eq_ignore_ascii_case("content-length") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                }
            }
            let mut b = vec![0u8; len];
            let _ = r.read_exact(&mut b);
            s2.lock().unwrap().push((path.clone(), auth));
            let (code, body) = srv.reply(&path);
            let mut c = conn;
            let _ = write!(
                c,
                "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (url, seen)
}

/// Строка ассистента Claude Code: `usage`, блоки `content` (тип и текст-приманка).
fn claude_line(session: &str, msg: &str, usage: [u64; 4], blocks: &[&str]) -> Value {
    let content: Vec<Value> = blocks
        .iter()
        .map(|t| match *t {
            "tool_use" => json!({"type": "tool_use", "id": "tu", "name": "Bash",
                                 "input": {"command": BAIT}}),
            _ => json!({"type": "text", "text": BAIT}),
        })
        .collect();
    json!({"type": "assistant", "sessionId": session, "uuid": format!("{msg}-{}", blocks.len()),
           "message": {"id": msg, "role": "assistant", "model": "m", "content": content,
                       "usage": {"input_tokens": usage[0],
                                 "cache_read_input_tokens": usage[1],
                                 "cache_creation_input_tokens": usage[2],
                                 "output_tokens": usage[3],
                                 "service_tier": "standard"}}})
}

fn write_jsonl(p: &Path, lines: &[Value]) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    let mut s = String::new();
    for l in lines {
        s.push_str(&l.to_string());
        s.push('\n');
    }
    fs::write(p, s).unwrap();
}

/// Снимок каталога: относительный путь → содержимое (проверка «файлы не меняются и не
/// появляются»).
fn snapshot(d: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![d.to_path_buf()];
    while let Some(x) = stack.pop() {
        for e in fs::read_dir(&x).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let rel = p
                    .strip_prefix(d)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, fs::read(&p).unwrap());
            }
        }
    }
    out
}

fn approx(a: &Value, b: f64) -> bool {
    a.as_f64().is_some_and(|x| (x - b).abs() < 1e-9)
}

/// §17 п. 16: две задачи × два плеча (`memory`, `off`) × повтор; транскрипты Claude Code (с
/// повтором строки одного сообщения на блок и субагентом в отдельном файле той же сессии) и
/// Codex; память — из `GET /sessions/{id}` (у плеча `off` сессии на сервере нет — 0). Отчёт по
/// плечам: токены входа, кэша, выхода, памяти, вызовы инструментов, доля успешных, интервалы
/// бутстрепа по задачам; сравнение плеч — `R` и Δ доли успешных с интервалами.
#[test]
fn ab_report_on_fixture() {
    let d = tmp("fixture");
    let tr = d.join("transcripts");
    // t1/memory/1 — Claude: сообщение m1 записано двумя строками (по блоку), m2 — третьей;
    // субагент той же сессии — отдельным файлом; строка пользователя без usage
    write_jsonl(
        &tr.join("proj/s-t1-mem.jsonl"),
        &[
            json!({"type": "user", "sessionId": "s-t1-mem", "message": {"role": "user", "content": BAIT}}),
            claude_line("s-t1-mem", "m1", [10, 100, 20, 5], &["text"]),
            claude_line("s-t1-mem", "m1", [10, 100, 20, 5], &["tool_use"]),
            claude_line("s-t1-mem", "m2", [5, 200, 0, 15], &["tool_use", "tool_use"]),
        ],
    );
    write_jsonl(
        &tr.join("proj/s-t1-mem/subagents/agent-a1.jsonl"),
        &[claude_line("s-t1-mem", "m9", [1, 10, 2, 3], &["tool_use"])],
    );
    // t1/off/1 — Claude
    write_jsonl(
        &tr.join("proj/s-t1-off.jsonl"),
        &[claude_line(
            "s-t1-off",
            "m1",
            [40, 300, 60, 20],
            &["tool_use", "text"],
        )],
    );
    // t2/memory/1 — Codex: накопленный `total_token_usage` (входа с кэшем), вызовы — `response_item`
    write_jsonl(
        &tr.join("codex/rollout-2026-11-08T10-00-00-s-t2-mem.jsonl"),
        &[
            json!({"type": "session_meta", "payload": {"id": "s-t2-mem", "instructions": BAIT}}),
            json!({"type": "response_item", "payload": {"type": "message", "content": [{"type": "input_text", "text": BAIT}]}}),
            json!({"type": "response_item", "payload": {"type": "function_call", "name": "shell", "arguments": BAIT}}),
            json!({"type": "event_msg", "payload": {"type": "token_count", "info": {
                "total_token_usage": {"input_tokens": 100, "cached_input_tokens": 60, "output_tokens": 10, "reasoning_output_tokens": 4, "total_tokens": 110},
                "last_token_usage": {"input_tokens": 100, "cached_input_tokens": 60, "output_tokens": 10, "reasoning_output_tokens": 4, "total_tokens": 110}}}}),
            json!({"type": "response_item", "payload": {"type": "custom_tool_call", "name": "apply_patch", "input": BAIT}}),
            json!({"type": "event_msg", "payload": {"type": "token_count", "info": {
                "total_token_usage": {"input_tokens": 250, "cached_input_tokens": 150, "output_tokens": 30, "reasoning_output_tokens": 9, "total_tokens": 280},
                "last_token_usage": {"input_tokens": 150, "cached_input_tokens": 90, "output_tokens": 20, "reasoning_output_tokens": 5, "total_tokens": 170}}}}),
            json!({"type": "event_msg", "payload": {"type": "token_count", "info": null}}),
        ],
    );
    // t2/off/1 — Claude
    write_jsonl(
        &tr.join("proj/s-t2-off.jsonl"),
        &[claude_line(
            "s-t2-off",
            "m1",
            [50, 400, 0, 50],
            &["tool_use"],
        )],
    );
    let runs = d.join("runs.toml");
    fs::write(
        &runs,
        r#"# метки прогонов: задача, плечо, повтор, сессия, исход
[[run]]
task = "t1"
arm = "memory"
rep = 1
session = "s-t1-mem"
outcome = "pass"

[[run]]
task = "t1"
arm = "off"
rep = 1
session = "s-t1-off"
outcome = "fail"

[[run]]
task = "t2"
arm = "memory"
rep = 1
session = "s-t2-mem"
outcome = "pass"

[[run]]
task = "t2"
arm = "off"
rep = 1
session = "s-t2-off"
outcome = "pass"

# пустышка: журнала нет — с --allow-missing в агрегаты не входит
[[run]]
task = "t1"
arm = "memory"
rep = 2
session = "s-no-transcript"
outcome = "fail"
"#,
    )
    .unwrap();
    let mut sessions = BTreeMap::new();
    sessions.insert(
        "s-t1-mem".to_string(),
        json!({"id": "s-t1-mem", "holdout": false, "memory_writes": 1,
               "tokens": {"session_start": {"calls": 1, "tokens": 70},
                          "prompt": {"calls": 2, "tokens": 30}}}),
    );
    sessions.insert(
        "s-t2-mem".to_string(),
        json!({"id": "s-t2-mem", "holdout": false, "memory_writes": 0,
               "tokens": {"session_start": {"calls": 1, "tokens": 50},
                          "mcp": {"calls": 3, "tokens": 25}}}),
    );
    let (url, seen) = stub(sessions);
    let out = d.join("report").join("ab.json");
    let before = snapshot(&tr);
    let mut c = Command::new(env!("CARGO_BIN_EXE_xtask"));
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("ZAM_") {
            c.env_remove(k);
        }
    }
    let o = c
        .env("ZAM_TOKEN", "zam_test")
        .args(["ab", "--transcripts"])
        .arg(&tr)
        .arg("--runs")
        .arg(&runs)
        .args(["--allow-missing", "--server", &url, "--out"])
        .arg(&out)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&o.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&o.stderr).into_owned();
    assert!(o.status.success(), "stdout:\n{stdout}\nstderr:\n{stderr}");

    // файлы транскриптов не изменены, рядом ничего не появилось; в каталоге теста — только
    // фикстура и отчёт
    assert_eq!(snapshot(&tr), before, "каталог транскриптов изменился");
    let mut all: Vec<String> = snapshot(&d).into_keys().collect();
    all.retain(|p| !p.starts_with("transcripts/"));
    assert_eq!(all, ["report/ab.json", "runs.toml"], "лишние файлы");
    // текст сообщений в отчёт не попадает
    let report = fs::read_to_string(&out).unwrap();
    for (what, s) in [("stdout", &stdout), ("stderr", &stderr), ("отчёт", &report)] {
        assert!(!s.contains(BAIT), "{what} содержит текст транскрипта");
    }
    // сервер: возможности (`/health`), `GET /sessions/{id}` и субагенты (`?parent=`) прогонов,
    // выдача без сессии за 30 суток (M7 §5.8 п. 4) — все с токеном
    let seen = seen.lock().unwrap().clone();
    let mut paths: Vec<&str> = seen.iter().map(|(p, _)| p.as_str()).collect();
    paths.sort();
    assert_eq!(
        paths,
        [
            "/api/v1/health",
            "/api/v1/sessions/s-t1-mem",
            "/api/v1/sessions/s-t1-off",
            "/api/v1/sessions/s-t2-mem",
            "/api/v1/sessions/s-t2-off",
            "/api/v1/sessions/unattributed?days=30",
            "/api/v1/sessions?parent=s-t1-mem&tokens=true&limit=500",
            "/api/v1/sessions?parent=s-t1-off&tokens=true&limit=500",
            "/api/v1/sessions?parent=s-t2-mem&tokens=true&limit=500",
            "/api/v1/sessions?parent=s-t2-off&tokens=true&limit=500",
        ]
    );
    assert!(seen.iter().all(|(_, a)| a == "Bearer zam_test"), "{seen:?}");

    let j: Value = serde_json::from_str(&report).unwrap();
    assert_eq!(j["runs"], 4, "{j}");
    assert_eq!(j["tasks"], 2, "{j}");
    let runs_j = j["run"].as_array().unwrap();
    let run = |s: &str| runs_j.iter().find(|r| r["session"] == s).unwrap().clone();
    // Claude: m1 — один раз (повтор строки на блок), m2, субагент m9
    let r = run("s-t1-mem");
    assert_eq!(r["input"], 16, "{r}");
    assert_eq!(r["cache_read"], 310, "{r}");
    assert_eq!(r["cache_creation"], 22, "{r}");
    assert_eq!(r["output"], 23, "{r}");
    assert_eq!(r["tool_calls"], 4, "{r}");
    assert_eq!(r["memory"], 100, "{r}");
    // субагентов на сервере нет: память сессии — та же
    assert_eq!(r["memory_session"], 100, "{r}");
    assert_eq!(r["subagents"], 0, "{r}");
    assert_eq!(r["transcripts"], 2, "{r}");
    // Codex: итог накопленного, вход без кэша
    let r = run("s-t2-mem");
    assert_eq!(r["input"], 100, "{r}");
    assert_eq!(r["cache_read"], 150, "{r}");
    assert_eq!(r["cache_creation"], 0, "{r}");
    assert_eq!(r["output"], 30, "{r}");
    assert_eq!(r["tool_calls"], 2, "{r}");
    assert_eq!(r["memory"], 75, "{r}");
    // плечо off: сессий на сервере нет — память 0
    assert_eq!(run("s-t1-off")["memory"], 0);
    assert_eq!(run("s-t2-off")["tool_calls"], 1);

    let arm = |a: &str| j["arms"][a].clone();
    let m = arm("memory");
    assert_eq!(m["runs"], 2, "{m}");
    assert_eq!(m["tasks"], 2, "{m}");
    // суммы по плечу
    assert_eq!(m["sum"]["input"], 116, "{m}");
    assert_eq!(m["sum"]["output"], 53, "{m}");
    assert_eq!(m["sum"]["memory"], 175, "{m}");
    assert_eq!(m["sum"]["tool_calls"], 6, "{m}");
    // среднее по задачам с интервалом: total t1 = 16+310+22+23 = 371, t2 = 280 → 325,5
    assert!(approx(&m["per_task"]["total"]["mean"], 325.5), "{m}");
    let lo = m["per_task"]["total"]["lo"].as_f64().unwrap();
    let hi = m["per_task"]["total"]["hi"].as_f64().unwrap();
    assert!(
        lo <= 325.5 && 325.5 <= hi && lo >= 280.0 && hi <= 371.0,
        "{m}"
    );
    assert!(approx(&m["pass_rate"]["mean"], 1.0), "{m}");
    assert!(approx(&arm("off")["pass_rate"]["mean"], 0.5));
    // сравнение memory против off: R = Σ memory / Σ off по задачам
    let cmp = j["compare"].as_array().unwrap();
    assert_eq!(cmp.len(), 1, "{j}");
    let c0 = &cmp[0];
    assert_eq!(c0["base"], "off");
    assert_eq!(c0["arm"], "memory");
    assert_eq!(c0["tasks"], 2);
    // off: t1 = 40+300+60+20 = 420, t2 = 50+400+0+50 = 500; memory 371 + 280 = 651
    assert!(approx(&c0["r_total"]["mean"], 651.0 / 920.0), "{c0}");
    assert!(
        c0["r_total"]["lo"].as_f64().unwrap() <= c0["r_total"]["hi"].as_f64().unwrap(),
        "{c0}"
    );
    assert!(approx(&c0["d_pass_rate"]["mean"], 0.5), "{c0}");
    // пустышка — отдельно, агрегаты выше её не учитывают (memory: 2 прогона, доля успешных 1,0)
    assert_eq!(
        j["missing"],
        json!([{"task": "t1", "arm": "memory", "rep": 2, "session": "s-no-transcript",
                "outcome": "fail"}]),
        "{j}"
    );
    assert!(stdout.contains("Без журнала"), "{stdout}");
    // выдачи без сессии нет: раздел есть, предупреждения нет
    assert_eq!(
        j["unattributed"],
        json!({"days": 30, "calls": 0, "tokens": 0, "by_event": {}}),
        "{j}"
    );
    assert!(
        stdout.contains("## Выдано без сессии за 30 сут."),
        "{stdout}"
    );
    assert_eq!(j["warnings"], json!([]), "{j}");
    // вывод по плечам — с интервалами
    for want in ["memory", "off", "R memory / off", "[", "доля успешных"] {
        assert!(stdout.contains(want), "нет «{want}»:\n{stdout}");
    }
}

/// Две метки (`t1`: `memory` — сессия `s-m`, `off` — `s-o`) и их транскрипты Claude Code в `d`.
fn two_runs(d: &Path) {
    write_jsonl(
        &d.join("tr/s-m.jsonl"),
        &[claude_line("s-m", "m1", [10, 20, 30, 40], &["tool_use"])],
    );
    write_jsonl(
        &d.join("tr/s-o.jsonl"),
        &[claude_line("s-o", "m1", [1, 2, 3, 4], &["text"])],
    );
    fs::write(
        d.join("runs.toml"),
        "[[run]]\ntask = \"t1\"\narm = \"memory\"\nrep = 1\nsession = \"s-m\"\noutcome = \"pass\"\n\n\
         [[run]]\ntask = \"t1\"\narm = \"off\"\nrep = 1\nsession = \"s-o\"\noutcome = \"fail\"\n",
    )
    .unwrap();
}

/// `xtask ab` по фикстуре [`two_runs`] каталога `d` с сервером `url` (токен — `ZAM_TOKEN`) и
/// аргументами `extra`: вывод и JSON-отчёт (`d/ab.json`).
fn run_ab(d: &Path, url: Option<&str>, extra: &[&str]) -> (Output, Value) {
    let out = d.join("ab.json");
    let _ = fs::remove_file(&out);
    let mut c = Command::new(env!("CARGO_BIN_EXE_xtask"));
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("ZAM_") {
            c.env_remove(k);
        }
    }
    c.env("ZAM_TOKEN", "zam_test")
        .args(["ab", "--transcripts"])
        .arg(d.join("tr"))
        .arg("--runs")
        .arg(d.join("runs.toml"))
        .arg("--out")
        .arg(&out)
        .args(extra);
    if let Some(u) = url {
        c.args(["--server", u]);
    }
    let o = c.output().unwrap();
    assert!(
        o.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    let j = serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
    (o, j)
}

/// M7 §5.8 п. 4, решение координатора 27: память прогона — сессия и её субагенты
/// (`GET /sessions?parent=`): `memory` — сумма, `memory_session` — только сессия (как в отчётах
/// 0.0.14), `subagents` — их число; субагенты чужой сессии не входят. Сервер без признака
/// `sessions_parent` — один обход `GET /sessions?limit=500&tokens=true` по `parent_id` и
/// предупреждение; `/health` не ответил — пробный `?parent=`, на 400 — тот же обход.
#[test]
fn ab_memory_includes_subagents() {
    let d = tmp("subagents");
    two_runs(&d);
    let mut sessions = BTreeMap::new();
    sessions.insert(
        "s-m".to_string(),
        json!({"id": "s-m", "holdout": false,
               "tokens": {"session_start": {"calls": 1, "tokens": 100}}}),
    );
    let subagents = vec![
        json!({"id": "s-m:a1", "parent_id": "s-m", "agent_type": "Explore",
               "tokens": {"subagent_local": {"calls": 1, "tokens": 40}}}),
        json!({"id": "s-m:a2", "parent_id": "s-m", "agent_type": "Plan",
               "tokens": {"mcp": {"calls": 2, "tokens": 10}, "pre_tool": {"calls": 1, "tokens": 5}}}),
        // субагент сессии вне прогонов
        json!({"id": "s-x:a9", "parent_id": "s-x", "tokens": {"mcp": {"calls": 1, "tokens": 999}}}),
    ];
    let check = |j: &Value, what: &str| {
        let runs = j["run"].as_array().unwrap();
        let run = |s: &str| runs.iter().find(|r| r["session"] == s).unwrap().clone();
        let m = run("s-m");
        assert_eq!(m["memory"], 155, "{what}: {m}");
        assert_eq!(m["memory_session"], 100, "{what}: {m}");
        assert_eq!(m["subagents"], 2, "{what}: {m}");
        let o = run("s-o");
        assert_eq!((&o["memory"], &o["memory_session"]), (&json!(0), &json!(0)));
        let arm = &j["arms"]["memory"];
        assert_eq!(arm["sum"]["memory"], 155, "{what}: {arm}");
        assert_eq!(arm["sum"]["memory_session"], 100, "{what}: {arm}");
    };
    let paths = |seen: &Seen| -> Vec<String> {
        seen.lock()
            .unwrap()
            .iter()
            .map(|(p, _)| p.clone())
            .collect()
    };
    let parent_warning = |j: &Value| {
        j["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .filter(|w| w.contains("sessions_parent"))
            .count()
    };

    // сервер M7.2: `?parent=` на каждый прогон, обхода списка нет
    let (url, seen) = stub_server(Server {
        subagents: subagents.clone(),
        ..Server::current(sessions.clone())
    });
    let (o, j) = run_ab(&d, Some(&url), &[]);
    check(&j, "sessions_parent");
    assert_eq!(parent_warning(&j), 0, "{j}");
    let p = paths(&seen);
    assert!(
        p.contains(&"/api/v1/sessions?parent=s-m&tokens=true&limit=500".to_string()),
        "{p:?}"
    );
    assert!(
        !p.iter()
            .any(|x| x.contains("limit=500") && !x.contains("parent=")),
        "{p:?}"
    );
    let stdout = String::from_utf8_lossy(&o.stdout);
    assert!(stdout.contains("память сессии"), "{stdout}");

    // сервер без признака: `?parent=` не шлётся, один обход списка, предупреждение
    let (url, seen) = stub_server(Server {
        subagents: subagents.clone(),
        features: Some(vec!["m6", "unattributed"]),
        ..Server::current(sessions.clone())
    });
    let (o, j) = run_ab(&d, Some(&url), &[]);
    check(&j, "без признака");
    assert_eq!(parent_warning(&j), 1, "{j}");
    let p = paths(&seen);
    assert!(!p.iter().any(|x| x.contains("parent=")), "{p:?}");
    assert_eq!(
        p.iter()
            .filter(|x| *x == "/api/v1/sessions?limit=500&tokens=true")
            .count(),
        1,
        "{p:?}"
    );
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(
        stderr.contains("предупреждение:") && stderr.contains("sessions_parent"),
        "{stderr}"
    );

    // `/health` не ответил (возможности неизвестны), сервер без `parent` — 400 на пробу и обход
    let (url, seen) = stub_server(Server {
        subagents,
        features: None,
        unattributed: None,
        ..Server::current(sessions)
    });
    let (_, j) = run_ab(&d, Some(&url), &[]);
    check(&j, "400");
    assert_eq!(parent_warning(&j), 1, "{j}");
    let p = paths(&seen);
    assert_eq!(
        p.iter().filter(|x| x.contains("parent=")).count(),
        1,
        "{p:?}"
    );
    // у такого сервера нет и учёта без сессии: `unattributed: null` с предупреждением
    assert!(j["unattributed"].is_null(), "{j}");
    assert!(
        j["warnings"].as_array().unwrap().iter().any(|w| w
            .as_str()
            .is_some_and(|w| w.contains("GET /sessions/unattributed"))),
        "{j}"
    );
}

/// M7 §5.8 п. 4: `--unattributed-days N` (30) — раздел «Выдано без сессии за N сут.» вне плеч
/// (`GET /sessions/unattributed?days=N`: вызовы и токены по событиям) и при T > 0 предупреждение
/// «часть выдачи памяти не отнесена к прогонам: MCP без сессии»; T = 0 — раздел без
/// предупреждения; без `--server` — `unattributed: null`, раздела нет; N вне 1..=366 — ошибка.
#[test]
fn ab_unattributed_section() {
    let d = tmp("unattributed");
    two_runs(&d);
    let rows = vec![
        json!({"day": "2026-11-22", "event": "brief", "calls": 1, "tokens": 5}),
        json!({"day": "2026-11-22", "event": "mcp", "calls": 3, "tokens": 120}),
        json!({"day": "2026-11-20", "event": "mcp", "calls": 1, "tokens": 30}),
    ];
    let (url, seen) = stub_server(Server {
        unattributed: Some(rows),
        ..Server::current(BTreeMap::new())
    });
    let (o, j) = run_ab(&d, Some(&url), &["--unattributed-days", "7"]);
    assert_eq!(
        j["unattributed"],
        json!({"days": 7, "calls": 5, "tokens": 155, "by_event": {
            "brief": {"calls": 1, "tokens": 5}, "mcp": {"calls": 4, "tokens": 150}}}),
        "{j}"
    );
    let p: Vec<String> = seen
        .lock()
        .unwrap()
        .iter()
        .map(|(p, _)| p.clone())
        .collect();
    assert!(
        p.contains(&"/api/v1/sessions/unattributed?days=7".to_string()),
        "{p:?}"
    );
    let stdout = String::from_utf8_lossy(&o.stdout);
    let section = stdout
        .split_once("## Выдано без сессии за 7 сут.\n")
        .unwrap_or_else(|| panic!("{stdout}"))
        .1;
    assert!(section.contains("вызовов 5, токенов 155"), "{stdout}");
    assert!(section.contains("| mcp | 4 | 150 |"), "{stdout}");
    assert!(section.contains("| brief | 1 | 5 |"), "{stdout}");
    let warn = "часть выдачи памяти не отнесена к прогонам: MCP без сессии";
    assert!(
        j["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().is_some_and(|w| w.contains(warn))),
        "{j}"
    );
    assert!(String::from_utf8_lossy(&o.stderr).contains(warn));
    // раздел — вне плеч: память прогонов прежняя
    assert_eq!(j["arms"]["memory"]["sum"]["memory"], 0, "{j}");

    // выдачи без сессии нет — раздел без предупреждения
    let (url, _) = stub(BTreeMap::new());
    let (o, j) = run_ab(&d, Some(&url), &[]);
    assert_eq!(j["unattributed"]["days"], 30, "{j}");
    assert_eq!(j["unattributed"]["tokens"], 0, "{j}");
    assert!(
        String::from_utf8_lossy(&o.stdout).contains("## Выдано без сессии за 30 сут."),
        "{j}"
    );
    assert_eq!(j["warnings"], json!([]), "{j}");

    // без сервера — неизвестно
    let (o, j) = run_ab(&d, None, &[]);
    assert!(j["unattributed"].is_null(), "{j}");
    assert!(!String::from_utf8_lossy(&o.stdout).contains("без сессии"));

    // окно вне 1..=366 — ошибка разбора до сети
    for bad in ["0", "367"] {
        let o = xtask(&[
            "ab",
            "--transcripts",
            d.join("tr").to_str().unwrap(),
            "--runs",
            d.join("runs.toml").to_str().unwrap(),
            "--unattributed-days",
            bad,
        ]);
        assert!(!o.status.success(), "{bad}");
        assert!(
            String::from_utf8_lossy(&o.stderr).contains("--unattributed-days"),
            "{bad}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }
}

/// Прогон без транскрипта — ошибка; с `--allow-missing` — в `missing`, в агрегаты не входит;
/// неизвестный исход — ошибка до сети.
#[test]
fn ab_missing_transcript_and_bad_outcome() {
    let d = tmp("missing");
    let tr = d.join("tr");
    write_jsonl(
        &tr.join("a.jsonl"),
        &[claude_line("s-a", "m1", [1, 2, 3, 4], &["text"])],
    );
    let runs = d.join("runs.toml");
    fs::write(
        &runs,
        "[[run]]\ntask = \"t1\"\narm = \"memory\"\nrep = 1\nsession = \"s-a\"\noutcome = \"pass\"\n\n\
         [[run]]\ntask = \"t1\"\narm = \"off\"\nrep = 1\nsession = \"s-none\"\noutcome = \"fail\"\n",
    )
    .unwrap();
    let out = d.join("ab.json");
    let base = [
        "ab",
        "--transcripts",
        tr.to_str().unwrap(),
        "--runs",
        runs.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ];
    // без --allow-missing — ошибка с id сессии, отчёта нет
    let o = xtask(&base);
    assert!(!o.status.success());
    let e = String::from_utf8_lossy(&o.stderr);
    assert!(e.contains("s-none") && e.contains("--allow-missing"), "{e}");
    assert!(!out.exists());
    let mut args = base.to_vec();
    args.push("--allow-missing");
    let o = xtask(&args);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let j: Value = serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(j["missing"][0]["session"], "s-none", "{j}");
    assert_eq!(j["runs"], 1, "{j}");
    // плечо только из пустышек в агрегатах не появляется
    assert!(j["arms"].get("off").is_none(), "{j}");
    // без --server память не известна
    assert!(j["run"][0]["memory"].is_null(), "{j}");
    fs::write(
        &runs,
        "[[run]]\ntask = \"t1\"\narm = \"memory\"\nrep = 1\nsession = \"s-a\"\noutcome = \"ok\"\n",
    )
    .unwrap();
    let o = xtask(&[
        "ab",
        "--transcripts",
        tr.to_str().unwrap(),
        "--runs",
        runs.to_str().unwrap(),
    ]);
    assert!(!o.status.success());
    let e = String::from_utf8_lossy(&o.stderr);
    assert!(e.contains("outcome"), "{e}");
}

/// Шаблоны стенда в репозитории: `eval/ab/tasks.toml` — ≥ 10 задач с проверочной командой и
/// плечи §12; `eval/ab/runs.toml` разбирается (меток владельца ещё нет).
#[test]
fn ab_templates_valid() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let t: toml::Value =
        toml::from_str(&fs::read_to_string(root.join("eval/ab/tasks.toml")).unwrap()).unwrap();
    let tasks = t["task"].as_array().unwrap();
    assert!(tasks.len() >= 10, "{}", tasks.len());
    let mut ids = std::collections::HashSet::new();
    for x in tasks {
        let id = x["id"].as_str().unwrap();
        assert!(ids.insert(id.to_string()), "повтор {id}");
        assert!(!x["prompt"].as_str().unwrap().trim().is_empty(), "{id}");
        assert!(!x["check"].as_str().unwrap().trim().is_empty(), "{id}");
    }
    let arms: Vec<&str> = t["arms"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap())
        .collect();
    for a in ["memory", "off", "map", "nomap", "impact", "noimpact"] {
        assert!(arms.contains(&a), "нет плеча {a}: {arms:?}");
    }
    let r: toml::Value =
        toml::from_str(&fs::read_to_string(root.join("eval/ab/runs.toml")).unwrap()).unwrap();
    assert!(r.get("run").is_none_or(|v| v.is_array()));
}
