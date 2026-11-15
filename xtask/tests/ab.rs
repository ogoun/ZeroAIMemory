//! Стенд агентного A/B `cargo xtask ab` (спецификация M6 §12, §17 п. 16; задача 12): отчёт по
//! плечам с интервалами на синтетической фикстуре — два транскрипта Claude Code и один Codex
//! (только поля, которые читает стенд, и посторонний текст-приманка), ответы `GET /sessions/{id}`
//! заглушки и `runs.toml`. Из транскриптов читаются только `usage` (и тип блоков — счёт вызовов
//! инструментов); файлы не копируются и не меняются — проверка тестом. Журналы агентов владельца
//! тест не читает.

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

/// Заглушка сервера: `GET /api/v1/sessions/{id}` → ответ из `sessions` (нет — 404); принятые
/// пути и заголовки авторизации — в журнал.
fn stub(sessions: BTreeMap<String, Value>) -> (String, Seen) {
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
            let id = path.strip_prefix("/api/v1/sessions/").unwrap_or("");
            let (code, body) = match sessions.get(id) {
                Some(v) => (200, v.to_string()),
                None => (404, r#"{"status":404,"detail":"нет"}"#.to_string()),
            };
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
    // сервер: только `GET /sessions/{id}` прогонов, с токеном
    let seen = seen.lock().unwrap().clone();
    let mut paths: Vec<&str> = seen.iter().map(|(p, _)| p.as_str()).collect();
    paths.sort();
    assert_eq!(
        paths,
        [
            "/api/v1/sessions/s-t1-mem",
            "/api/v1/sessions/s-t1-off",
            "/api/v1/sessions/s-t2-mem",
            "/api/v1/sessions/s-t2-off"
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
    // вывод по плечам — с интервалами
    for want in ["memory", "off", "R memory / off", "[", "доля успешных"] {
        assert!(stdout.contains(want), "нет «{want}»:\n{stdout}");
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
