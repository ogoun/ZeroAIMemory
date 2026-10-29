//! Хуки `zam hook …` (§10.3; задача 10 M4) против настоящего zam-server в том же процессе тестов:
//! вывод по контракту Claude Code и Codex, локальный кэш триггеров (ETag), один показ за сессию,
//! напоминание `checkpoint` (R1), переключатели (R5), fail-open и время (R3: `pre-tool` ≤ 200 мс
//! p95, остальные ≤ 1 с). Сервер без моделей (поиск — FTS). Каталоги проекта и «домашний» — во
//! временном каталоге целей сборки (`CARGO_TARGET_TMPDIR`), не в профиле пользователя.
mod common;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Output;
use std::time::{Duration, Instant};

use assert_cmd::Command;
use serde_json::{Value, json};
use zam_server::build;
use zam_server::config::ServerConfig;
use zam_store::Access;

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e61";
/// Адрес, где заведомо никто не слушает (порт discard).
const DEAD: &str = "http://127.0.0.1:9";
/// Запас времени R3: `pre-tool` — ×4 от 50 мс.
const PRE_TOOL_P95_MAX: Duration = Duration::from_millis(200);
/// Предел времени R3 по команде (fix round 1, m-1): ×4 от бюджета §10.3 — `checkpoint`
/// 400 мс, `session-start` 1200 мс, `prompt` и `post-tool` 1000 мс.
fn max_for(kind: &str) -> Duration {
    Duration::from_millis(match kind {
        "checkpoint" => 400,
        "session-start" => 1200,
        _ => 1000,
    })
}
/// Предел для прочих замеров процесса (без сети, огромный ввод).
const OTHER_MAX: Duration = Duration::from_secs(1);

/// Тесты файла идут по одному: замеры времени (R3) и бюджеты хуков (270 мс у
/// `session-start`, 70 мс у `checkpoint`) не должны зависеть от соседних тестов.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// HTTP-клиент тестов без системного прокси.
fn http() -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .proxy(None)
            .http_status_as_error(false)
            .build(),
    )
}

fn tmp(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("cli-hooks-{name}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Сервер в фоновом потоке: адрес и токен `rw` пользователя «Анна».
fn start(name: &str) -> (String, String) {
    let data = tmp(&format!("{name}-data"));
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
            // отладочная сборка на HDD: сервер не обрезает сбор (бюджет — у клиента)
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
    // прогрев: база пользователя и первые запросы отладочной сборки — до замеров и бюджетов
    let a = http();
    let auth = format!("Bearer {tok}");
    for _ in 0..2 {
        let _ = a
            .post(&format!("{url}/api/v1/recall"))
            .header("Authorization", &auth)
            .content_type("application/json")
            .send(r#"{"event":"session_start","session_id":"warmup"}"#);
        let _ = a
            .get(&format!("{url}/api/v1/sessions/warmup"))
            .header("Authorization", &auth)
            .call();
        let _ = a
            .get(&format!("{url}/api/v1/triggers"))
            .header("Authorization", &auth)
            .call();
    }
    (url, tok)
}

/// Проект с решением и «домашний» каталог.
struct Proj {
    url: String,
    token: String,
    repo: PathBuf,
    home: PathBuf,
}

impl Proj {
    fn new(name: &str, url: &str, token: &str) -> Proj {
        let root = tmp(name);
        let repo = root.join("repo");
        let home = root.join("home");
        std::fs::create_dir_all(repo.join(".zeroaimemory")).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            repo.join(".zeroaimemory/solution.toml"),
            format!("agent_solution_id = \"{G}\"\nname = \"Хуки\"\n"),
        )
        .unwrap();
        Proj {
            url: url.into(),
            token: token.into(),
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
            .env("COMPUTERNAME", "hook-box")
            .env("HOSTNAME", "hook-box")
            .current_dir(&self.repo);
        c
    }

    /// `zam <args>` с stdin.
    fn zam(&self, args: &[&str], stdin: &[u8]) -> Output {
        self.cmd(&self.url)
            .args(args)
            .write_stdin(stdin.to_vec())
            .output()
            .unwrap()
    }

    /// `zam hook <kind> [--agent codex]` с JSON на stdin: вывод и время процесса.
    fn hook_at(&self, url: &str, kind: &str, agent: &str, stdin: &[u8]) -> (Output, Duration) {
        let mut c = self.cmd(url);
        c.args(["hook", kind, "--agent", agent])
            .write_stdin(stdin.to_vec());
        let t = Instant::now();
        let o = c.output().unwrap();
        (o, t.elapsed())
    }

    fn hook(&self, kind: &str, v: &Value) -> Output {
        self.hook_at(&self.url, kind, "claude", v.to_string().as_bytes())
            .0
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

    fn cache(&self) -> PathBuf {
        self.repo.join(".zeroaimemory/cache")
    }

    /// Файл кэша сессии: `<prefix>-<сессия>-<8 hex>[<suffix>]` (имя с хэшем id, m-9).
    fn session_file(&self, prefix: &str, session: &str, suffix: &str) -> Option<PathBuf> {
        let start = format!("{prefix}-{session}-");
        std::fs::read_dir(self.cache())
            .ok()?
            .flatten()
            .find_map(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                let rest = n.strip_prefix(&start)?.strip_suffix(suffix)?;
                (rest.len() == 8 && rest.chars().all(|c| c.is_ascii_hexdigit())).then(|| e.path())
            })
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.repo.join(".zeroaimemory/logs/hooks.log")).unwrap_or_default()
    }

    /// `zam log insight` с триггерами; id записи.
    fn insight(&self, title: &str, kind: &str, scope: &str, triggers: &[&str]) -> String {
        let mut args = vec![
            "log", "insight", "--kind", kind, "--title", title, "--scope", scope,
        ];
        for t in triggers {
            args.push("--trigger");
            args.push(t);
        }
        let o = self.zam(&args, "Разобрано на практике.\n".as_bytes());
        id_of(&ok(&o))
    }

    /// `GET /sessions/{id}` через сервер.
    fn session(&self, id: &str) -> Value {
        let r = http()
            .get(&format!("{}/api/v1/sessions/{id}", self.url))
            .header("Authorization", format!("Bearer {}", self.token))
            .call()
            .unwrap();
        serde_json::from_str(&r.into_body().read_to_string().unwrap()).unwrap()
    }
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

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

fn id_of(s: &str) -> String {
    s.trim().split('@').next().unwrap().to_string()
}

/// Вывод хука: код 0 и JSON (или пусто → `Null`).
fn hook_json(o: &Output) -> Value {
    let s = ok(o);
    if s.trim().is_empty() {
        return Value::Null;
    }
    serde_json::from_str(s.trim()).unwrap_or_else(|e| panic!("не JSON ({e}): {s}"))
}

/// `additionalContext` вывода хука (событие проверяется).
fn context(o: &Output, event: &str) -> String {
    let v = hook_json(o);
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], event, "{v}");
    v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_else(|| panic!("нет additionalContext: {v}"))
        .to_string()
}

fn empty(o: &Output) {
    assert_eq!(ok(o).trim(), "", "ожидалась тишина");
}

fn p95(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[(v.len() * 95).div_ceil(100) - 1]
}

/// Кэш триггеров вручную: один `file_glob`, один `command_regex`, один `error_regex`.
fn write_cache(p: &Proj, server: &str) {
    let t = |id: &str, kind: &str, pattern: &str, title: &str| {
        json!({"id": format!("t{id}"), "entry_id": format!("E{id}"), "type": kind,
               "pattern": pattern, "tools": null, "title": title, "insight_kind": "gotcha",
               "certainty": "theorem", "space": "knowledge"})
    };
    let c = json!({
        "etag": "e1", "fetched_at": "2026-10-27T00:00:00Z", "server": server, "solution": G,
        "triggers": [
            t("1", "file_glob", "**/*.sh", "Скрипты — только LF"),
            t("2", "command_regex", "^git push --force", "Не форсить main"),
            t("3", "error_regex", "bad interpreter", "CRLF в shebang"),
        ],
    });
    std::fs::create_dir_all(p.cache()).unwrap();
    std::fs::write(p.cache().join("triggers.json"), c.to_string()).unwrap();
}

fn write_ev(p: &Proj, session: &str, file: &str) -> Value {
    p.ev(
        "PreToolUse",
        session,
        json!({"tool_name": "Write", "tool_use_id": "u1",
               "tool_input": {"file_path": p.repo.join(file), "content": "#!/bin/sh\n"}}),
    )
}

/// Сервер выключен: `pre-tool` сопоставляет маску по кэшу без сети; 100 запусков — p95 в
/// журнал этапа (R3: ≤ 200 мс).
#[test]
fn pre_tool_matches_glob_from_cache_without_network() {
    let _g = serial();
    let p = Proj::new("pre-offline", DEAD, "zam_x");
    write_cache(&p, DEAD);
    let (o, _) = p.hook_at(
        DEAD,
        "pre-tool",
        "claude",
        write_ev(&p, "s1", "scripts/deploy.sh")
            .to_string()
            .as_bytes(),
    );
    let c = context(&o, "PreToolUse");
    assert_eq!(
        c,
        "ZeroAIMemory: для **/*.sh: Скрипты — только LF [gotcha, theorem] — i:E1"
    );
    // команда — command_regex; не совпало — тишина
    let bash = |cmd: &str, s: &str| {
        p.ev(
            "PreToolUse",
            s,
            json!({"tool_name": "Bash", "tool_input": {"command": cmd}}),
        )
    };
    let (o, _) = p.hook_at(
        DEAD,
        "pre-tool",
        "claude",
        bash("git push --force origin", "s1").to_string().as_bytes(),
    );
    assert!(context(&o, "PreToolUse").contains("Не форсить main [gotcha, theorem] — i:E2"));
    let (o, _) = p.hook_at(
        DEAD,
        "pre-tool",
        "claude",
        bash("ls -la", "s1").to_string().as_bytes(),
    );
    empty(&o);
    let (o, _) = p.hook_at(
        DEAD,
        "pre-tool",
        "claude",
        write_ev(&p, "s1", "src/a.rs").to_string().as_bytes(),
    );
    empty(&o);
}

#[test]
fn pre_tool_under_50ms() {
    let _g = serial();
    let p = Proj::new("pre-timing", DEAD, "zam_x");
    write_cache(&p, DEAD);
    let mut times = Vec::new();
    for i in 0..100 {
        let v = write_ev(&p, &format!("t{i}"), "scripts/deploy.sh").to_string();
        let (o, d) = p.hook_at(DEAD, "pre-tool", "claude", v.as_bytes());
        assert!(context(&o, "PreToolUse").contains("i:E1"));
        times.push(d);
    }
    let q = p95(times.clone());
    let min = times.iter().min().unwrap();
    eprintln!("ZAM_TIMING pre-tool p95={q:?} min={min:?} (100 запусков)");
    assert!(q <= PRE_TOOL_P95_MAX, "pre-tool p95 {q:?}");
}

/// Запись показывается не больше раза за сессию; другая сессия — снова; Write отмечает правку.
#[test]
fn pre_tool_once_per_session() {
    let _g = serial();
    let p = Proj::new("pre-once", DEAD, "zam_x");
    write_cache(&p, DEAD);
    let run = |s: &str, f: &str| {
        p.hook_at(
            DEAD,
            "pre-tool",
            "claude",
            write_ev(&p, s, f).to_string().as_bytes(),
        )
        .0
    };
    assert!(context(&run("a", "x.sh"), "PreToolUse").contains("i:E1"));
    empty(&run("a", "y/z.sh"));
    assert!(context(&run("b", "x.sh"), "PreToolUse").contains("i:E1"));
    let shown: Value = serde_json::from_str(
        &std::fs::read_to_string(p.session_file("shown", "a", ".json").unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(shown["shown"], json!(["E1"]));
    // факт правки — отдельный маркер (m-2)
    assert!(p.session_file("edits", "a", "").is_some());
    // Bash правкой не считается
    let bash = p.ev(
        "PreToolUse",
        "c",
        json!({"tool_name": "Bash", "tool_input": {"command": "ls"}}),
    );
    p.hook_at(DEAD, "pre-tool", "claude", bash.to_string().as_bytes());
    assert!(p.session_file("edits", "c", "").is_none());
}

/// Пути `file_path` нормализуются (`.`, `..`); путь выше корня — не сопоставляется (fix round 2).
#[test]
fn pre_tool_normalizes_paths() {
    let _g = serial();
    let p = Proj::new("pre-norm", DEAD, "zam_x");
    write_cache(&p, DEAD);
    let run = |s: &str, f: &str| {
        p.hook_at(
            DEAD,
            "pre-tool",
            "claude",
            write_ev(&p, s, f).to_string().as_bytes(),
        )
        .0
    };
    assert!(context(&run("n1", "a/./b/../deploy.sh"), "PreToolUse").contains("i:E1"));
    empty(&run("n2", "../outside.sh"));
    empty(&run("n3", "a/../../outside.sh"));
}

/// Хук из подкаталога решения с `solution.toml` в монорепо: корень — каталог решения, а не
/// корень git (fix round 2).
#[test]
fn hook_root_is_solution_dir_in_monorepo() {
    let _g = serial();
    let p = Proj::new("monorepo", DEAD, "zam_x");
    // монорепо: .git наверху, решение M0 (`solution.toml`) — в svc/
    let mono = p.repo.join("mono");
    let svc = mono.join("svc");
    let cwd = svc.join("src");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::write(mono.join(".git"), "gitdir: x").unwrap();
    std::fs::write(
        svc.join("solution.toml"),
        format!("agent_solution_id = \"{G}\"\nname = \"svc\"\n"),
    )
    .unwrap();
    let c = json!({
        "etag": "e1", "fetched_at": "x", "server": DEAD, "solution": G,
        "triggers": [{"id": "t9", "entry_id": "E9", "type": "file_glob", "pattern": "scripts/*.sh",
                      "tools": null, "title": "Скрипты svc", "insight_kind": "gotcha",
                      "certainty": null, "space": "knowledge"}],
    });
    std::fs::create_dir_all(svc.join(".zeroaimemory/cache")).unwrap();
    std::fs::write(svc.join(".zeroaimemory/cache/triggers.json"), c.to_string()).unwrap();
    let ev = json!({
        "session_id": "m1", "cwd": cwd, "hook_event_name": "PreToolUse", "tool_name": "Write",
        "tool_input": {"file_path": svc.join("scripts/run.sh"), "content": "x"},
    });
    let (o, _) = p.hook_at(DEAD, "pre-tool", "claude", ev.to_string().as_bytes());
    assert!(context(&o, "PreToolUse").contains("для scripts/*.sh: Скрипты svc"));
    // без .zeroaimemory/ в svc корень находится по solution.toml
    std::fs::remove_dir_all(svc.join(".zeroaimemory")).unwrap();
    let ev2 = json!({
        "session_id": "m2", "cwd": cwd, "hook_event_name": "PreToolUse", "tool_name": "Write",
        "tool_input": {"file_path": svc.join("a.txt"), "content": "x"},
    });
    p.hook_at(DEAD, "pre-tool", "claude", ev2.to_string().as_bytes());
    let marker = std::fs::read_dir(svc.join(".zeroaimemory/cache"))
        .unwrap()
        .flatten()
        .any(|e| e.file_name().to_string_lossy().starts_with("edits-m2-"));
    assert!(marker, "маркер правок — в кэше каталога решения");
}

/// Codex `apply_patch` (I-3): пути — из заголовков патча, правка отмечается.
#[test]
fn pre_tool_codex_apply_patch_paths() {
    let _g = serial();
    let p = Proj::new("pre-patch", DEAD, "zam_x");
    write_cache(&p, DEAD);
    let patch = "*** Begin Patch\n*** Update File: src/main.rs\n@@\n-a\n+b\n*** Add File: scripts/deploy.sh\n+#!/bin/sh\n*** End Patch\n";
    let ev = p.ev(
        "PreToolUse",
        "px",
        json!({"tool_name": "apply_patch", "tool_use_id": "c1",
               "tool_input": {"command": patch}}),
    );
    let (o, _) = p.hook_at(DEAD, "pre-tool", "codex", ev.to_string().as_bytes());
    let c = context(&o, "PreToolUse");
    assert!(c.contains("для **/*.sh: Скрипты — только LF"), "{c}");
    // command_regex к тексту патча не применяется
    assert!(!c.contains("i:E2"), "{c}");
    assert!(p.session_file("edits", "px", "").is_some());
    // патч без подходящих файлов — тишина, но правка отмечена
    let ev = p.ev(
        "PreToolUse",
        "py",
        json!({"tool_name": "apply_patch",
               "tool_input": {"command": "*** Begin Patch\n*** Delete File: a.txt\n*** End Patch\n"}}),
    );
    empty(
        &p.hook_at(DEAD, "pre-tool", "codex", ev.to_string().as_bytes())
            .0,
    );
    assert!(p.session_file("edits", "py", "").is_some());
}

/// `session-start`: сессия на сервере, дайджест с ловушкой, кэш триггеров с ETag; повтор —
/// `If-None-Match` (304: кэш не переписывается составом), новый триггер — новый ETag.
#[test]
fn session_start_outputs_digest_and_refreshes_cache_with_etag() {
    let _g = serial();
    let (url, tok) = start("session-start");
    let p = Proj::new("session-start", &url, &tok);
    std::fs::write(
        p.repo.join("CLAUDE.md"),
        "<!-- zeroaimemory:begin v1 -->\nZeroAIMemoryUrl=http://x\n<!-- zeroaimemory:end -->\n",
    )
    .unwrap();
    let id = p.insight(
        "Скрипты — только LF",
        "gotcha",
        "solution",
        &["file_glob:**/*.sh"],
    );
    let ev = p.ev(
        "SessionStart",
        "ss-1",
        json!({"source": "startup", "model": "m"}),
    );
    let c = context(&p.hook("session-start", &ev), "SessionStart");
    assert!(
        c.contains(&format!(
            "Скрипты — только LF [gotcha, hypothesis] — i:{id}"
        )),
        "{c}"
    );
    // R2: блок v1 устарел — сервер напоминает
    assert!(c.contains("устарел (v1 → v2)"), "{c}");
    let s = p.session("ss-1");
    assert_eq!(s["agent"], "claude-code", "{s}");
    assert_eq!(s["solution"], G, "{s}");
    assert_eq!(s["machine"], "hook-box", "{s}");
    let path = p.cache().join("triggers.json");
    let read =
        || -> Value { serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap() };
    let c1 = read();
    assert_eq!(c1["server"], url.as_str());
    assert_eq!(c1["solution"], G);
    assert_eq!(c1["triggers"][0]["entry_id"], id.as_str(), "{c1}");
    let etag = c1["etag"].as_str().unwrap().to_string();
    assert!(!etag.is_empty());
    // 304: подменённый состав остаётся — значит, ответ без тела
    let mut tampered = c1.clone();
    tampered["triggers"] = json!([]);
    tampered["fetched_at"] = json!("2000-01-01T00:00:00Z");
    std::fs::write(&path, tampered.to_string()).unwrap();
    p.hook("session-start", &ev);
    let c2 = read();
    assert_eq!(c2["etag"], etag.as_str());
    assert_eq!(c2["triggers"], json!([]));
    assert_ne!(c2["fetched_at"], "2000-01-01T00:00:00Z");
    // новый триггер — новый ETag и состав
    p.insight(
        "Не форсить main",
        "failure",
        "global",
        &["command_regex:^git push --force"],
    );
    p.hook("session-start", &ev);
    let c3 = read();
    assert_ne!(c3["etag"], etag.as_str());
    assert_eq!(c3["triggers"].as_array().unwrap().len(), 2, "{c3}");
    // настройки сервера (R5) и текущая сессия для CLI
    let h: Value =
        serde_json::from_str(&std::fs::read_to_string(p.cache().join("hooks.json")).unwrap())
            .unwrap();
    assert_eq!(h["hooks"]["pre_tool"], true, "{h}");
    assert!(
        std::fs::read_to_string(p.cache().join("session.json"))
            .unwrap()
            .contains("ss-1")
    );
    // `zam triggers sync` — принудительно
    std::fs::write(&path, tampered.to_string()).unwrap();
    let o = p.zam(&["triggers", "sync"], b"");
    assert!(ok(&o).contains("триггеров: 2"), "{}", out(&o));
    assert_eq!(read()["triggers"].as_array().unwrap().len(), 2);
}

/// `PostToolUseFailure`: `error_regex` — с сервером и (сервер выключен) по кэшу.
#[test]
fn post_tool_failure_error_regex_hint() {
    let _g = serial();
    let (url, tok) = start("post-failure");
    let p = Proj::new("post-failure", &url, &tok);
    let id = p.insight(
        "CRLF в shebang",
        "failure",
        "global",
        &["error_regex:bad interpreter"],
    );
    let ev = p.ev(
        "PostToolUseFailure",
        "pf-1",
        json!({"tool_name": "Bash", "tool_input": {"command": "./deploy.sh"},
               "error": "Exit code 126\n/bin/bash^M: bad interpreter: No such file or directory"}),
    );
    let c = context(&p.hook("post-tool", &ev), "PostToolUseFailure");
    let line = format!(
        "ZeroAIMemory: эта ошибка уже разбиралась: CRLF в shebang [failure, hypothesis] — i:{id}"
    );
    assert!(c.contains(&line), "{c}");
    assert_eq!(c.matches(&format!("i:{id}")).count(), 1, "без дублей: {c}");
    // повторная ошибка — снова подсказка
    assert!(context(&p.hook("post-tool", &ev), "PostToolUseFailure").contains(&line));
    // кэш с тем же error_regex: локальная и серверная подсказки не дублируются
    p.hook(
        "session-start",
        &p.ev("SessionStart", "pf-1", json!({"source": "startup"})),
    );
    let c = context(&p.hook("post-tool", &ev), "PostToolUseFailure");
    assert_eq!(c.matches(&format!("i:{id}")).count(), 1, "без дублей: {c}");
    // сервер выключен: кэш того же адреса — локальная подсказка
    write_cache(&p, DEAD);
    let (o, d) = p.hook_at(DEAD, "post-tool", "claude", ev.to_string().as_bytes());
    let c = context(&o, "PostToolUseFailure");
    assert!(
        c.contains("эта ошибка уже разбиралась: CRLF в shebang [gotcha, theorem] — i:E3"),
        "{c}"
    );
    assert!(d <= OTHER_MAX, "{d:?}");
    // другая ошибка — тишина
    let other = p.ev(
        "PostToolUseFailure",
        "pf-1",
        json!({"tool_name": "Bash", "error": "permission denied"}),
    );
    empty(
        &p.hook_at(DEAD, "post-tool", "claude", other.to_string().as_bytes())
            .0,
    );
}

/// Codex: `PostToolUse` с непустым `tool_response.stderr`; пустой stderr — тишина.
#[test]
fn post_tool_codex_stderr_hint() {
    let _g = serial();
    let (url, tok) = start("post-codex");
    let p = Proj::new("post-codex", &url, &tok);
    let id = p.insight(
        "CRLF в shebang",
        "failure",
        "global",
        &["error_regex:bad interpreter"],
    );
    let ev = |stderr: &str| {
        p.ev(
            "PostToolUse",
            "pc-1",
            json!({"tool_name": "Bash", "tool_input": {"command": "./deploy.sh"},
                   "tool_response": {"stdout": "", "stderr": stderr}}),
        )
    };
    let (o, _) = p.hook_at(
        &url,
        "post-tool",
        "codex",
        ev("/bin/bash^M: bad interpreter").to_string().as_bytes(),
    );
    let c = context(&o, "PostToolUse");
    assert!(c.contains(&format!("i:{id}")), "{c}");
    let (o, _) = p.hook_at(&url, "post-tool", "codex", ev("").to_string().as_bytes());
    empty(&o);
}

/// `checkpoint`: напоминание одно за сессию и только после правок без записи в память;
/// `stop_hook_active` — тишина; запись через `zam` в сессии хуков снимает напоминание.
#[test]
fn checkpoint_reminds_once_and_respects_stop_hook_active() {
    let _g = serial();
    let (url, tok) = start("checkpoint");
    let p = Proj::new("checkpoint", &url, &tok);
    let start_s = |s: &str| {
        p.hook(
            "session-start",
            &p.ev("SessionStart", s, json!({"source": "startup"})),
        )
    };
    let stop = |s: &str, active: bool| {
        p.hook(
            "checkpoint",
            &p.ev(
                "Stop",
                s,
                json!({"stop_hook_active": active, "last_assistant_message": "x"}),
            ),
        )
    };
    start_s("cp-1");
    // правок не было — тишина
    empty(&stop("cp-1", false));
    p.hook("pre-tool", &write_ev(&p, "cp-1", "a.txt"));
    empty(&stop("cp-1", true));
    let v = hook_json(&stop("cp-1", false));
    assert_eq!(v["decision"], "block", "{v}");
    assert!(
        v["reason"].as_str().unwrap().contains("zam log changelog"),
        "{v}"
    );
    empty(&stop("cp-1", false));
    // PreCompact — всегда тишина (m-3); сервер выключен — тишина
    start_s("cp-2");
    p.hook("pre-tool", &write_ev(&p, "cp-2", "a.txt"));
    let pc = p.ev(
        "PreCompact",
        "cp-2",
        json!({"trigger": "auto", "custom_instructions": ""}),
    );
    empty(&p.hook("checkpoint", &pc));
    let st = p.ev("Stop", "cp-2", json!({"stop_hook_active": false}));
    empty(
        &p.hook_at(DEAD, "checkpoint", "claude", st.to_string().as_bytes())
            .0,
    );
    // напоминание не израсходовано ни PreCompact, ни выключенным сервером
    assert_eq!(hook_json(&stop("cp-2", false))["decision"], "block");
    // сессии на сервере нет (404), маркер правок есть — напоминание (fix round 2), одно
    p.hook("pre-tool", &write_ev(&p, "cp-unknown", "a.txt"));
    assert_eq!(hook_json(&stop("cp-unknown", false))["decision"], "block");
    empty(&stop("cp-unknown", false));
    // запись агентом через CLI в текущей сессии хуков (session.json) — напоминания нет
    start_s("cp-3");
    p.hook("pre-tool", &write_ev(&p, "cp-3", "a.txt"));
    let o = p
        .cmd(&p.url)
        .env("CLAUDECODE", "1")
        .args(["log", "changelog", "--kind", "fix", "--title", "Итог"])
        .write_stdin("Сделано.\n")
        .output()
        .unwrap();
    ok(&o);
    assert!(p.session("cp-3")["memory_writes"].as_i64().unwrap() >= 1);
    empty(&stop("cp-3", false));
}

/// Задача журнала (M4.5 §3): `in_progress`, захваченная сессией, и правки — единственное
/// напоминание просит обновить `next_step`, даже если в память уже писали (захват — запись).
/// Задача `open` напоминания не вызывает. Заодно — `zam task new|claim|status|show|list`.
#[test]
fn checkpoint_reminds_next_step_for_claimed_task() {
    let _g = serial();
    let (url, tok) = start("checkpoint-task");
    let p = Proj::new("checkpoint-task", &url, &tok);
    let start_s = |s: &str| {
        p.hook(
            "session-start",
            &p.ev("SessionStart", s, json!({"source": "startup"})),
        )
    };
    let stop = |s: &str| {
        p.hook(
            "checkpoint",
            &p.ev("Stop", s, json!({"stop_hook_active": false})),
        )
    };
    let task = |envs: bool, args: &[&str]| {
        let mut c = p.cmd(&p.url);
        if envs {
            c.env("CLAUDECODE", "1");
        }
        c.args(["task"])
            .args(args)
            .write_stdin(
                "План.
",
            )
            .output()
            .unwrap()
    };
    let new_task = |title: &str| {
        id_of(&ok(&task(
            false,
            &["new", "--global", "--title", title, "--next", "первый шаг"],
        )))
    };
    // сессия с задачей в работе
    start_s("cp-t1");
    let t1 = new_task("Рефакторинг");
    let k1 = format!("k:{t1}");
    ok(&task(true, &["claim", &k1, "--ttl", "30"]));
    ok(&task(
        true,
        &["status", &k1, "in_progress", "--next", "второй шаг"],
    ));
    let card = ok(&task(false, &["show", &k1]));
    assert!(card.contains("[in_progress] «Рефакторинг»"), "{card}");
    assert!(card.contains("захвачена: сессия cp-t1"), "{card}");
    assert!(card.contains("далее: второй шаг"), "{card}");
    assert!(ok(&task(false, &["list", "--global"])).contains(&t1));
    let j = p.session("cp-t1");
    assert_eq!(j["claimed_tasks"][0]["id"], t1.as_str(), "{j}");
    assert!(j["memory_writes"].as_i64().unwrap() >= 2, "{j}");
    // без правок — тишина; с правками — напоминание о next_step, один раз
    empty(&stop("cp-t1"));
    p.hook("pre-tool", &write_ev(&p, "cp-t1", "a.txt"));
    let v = hook_json(&stop("cp-t1"));
    assert_eq!(v["decision"], "block", "{v}");
    let reason = v["reason"].as_str().unwrap();
    assert!(
        reason.contains(&format!("обнови next_step задачи {k1} («Рефакторинг»)")),
        "{reason}"
    );
    assert!(
        !reason.contains("zam log changelog"),
        "в память уже писали: {reason}"
    );
    empty(&stop("cp-t1"));
    // захваченная, но не in_progress — напоминания нет
    start_s("cp-t2");
    let t2 = new_task("Только открыта");
    // без агентского окружения сессии нет — захват невозможен (400), задача свободна
    let o = task(false, &["claim", &format!("k:{t2}")]);
    assert_ne!(o.status.code(), Some(0));
    ok(&task(true, &["claim", &format!("k:{t2}")]));
    p.hook("pre-tool", &write_ev(&p, "cp-t2", "a.txt"));
    empty(&stop("cp-t2"));
    // чужая сессия: захваченная в cp-t1 задача занята — код 4
    let o = {
        let mut c = p.cmd(&p.url);
        c.env("CLAUDECODE", "1").env("ZAM_SESSION", "cp-t3");
        c.args(["task", "claim", &k1]).output().unwrap()
    };
    assert_eq!(
        o.status.code(),
        Some(4),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(String::from_utf8_lossy(&o.stderr).contains("cp-t1"));
}

/// I-1: `zam` подставляет сессию хуков, только если его запустил агент (`CLAUDECODE=1`,
/// переменные Codex, `ZAM_AGENT=1`); явный `ZAM_SESSION` сильнее файла, `ZAM_SESSION=-` — без
/// сессии; человек в терминале — без сессии.
#[test]
fn cli_session_only_when_launched_by_agent() {
    let _g = serial();
    let (url, tok) = start("cli-session");
    let p = Proj::new("cli-session", &url, &tok);
    p.hook(
        "session-start",
        &p.ev("SessionStart", "hs-1", json!({"source": "startup"})),
    );
    let writes = |id: &str| p.session(id)["memory_writes"].as_i64().unwrap_or(0);
    let log = |envs: &[(&str, &str)], title: &str| {
        let mut c = p.cmd(&p.url);
        for (k, v) in envs {
            c.env(k, v);
        }
        let o = c
            .args(["log", "changelog", "--kind", "fix", "--title", title])
            .write_stdin("x\n")
            .output()
            .unwrap();
        ok(&o);
    };
    // человек: сессии нет, хотя session.json свежий
    log(&[], "человек");
    assert_eq!(writes("hs-1"), 0);
    // агенты
    log(&[("CLAUDECODE", "1")], "claude");
    let w1 = writes("hs-1");
    assert!(w1 >= 1, "{w1}");
    log(&[("CODEX_SANDBOX", "seatbelt")], "codex");
    let w2 = writes("hs-1");
    assert!(w2 > w1, "{w2}");
    log(&[("ZAM_AGENT", "1")], "явно");
    let w3 = writes("hs-1");
    assert!(w3 > w2, "{w3}");
    // ZAM_SESSION сильнее файла; «-» — без сессии
    log(
        &[("CLAUDECODE", "1"), ("ZAM_SESSION", "explicit-1")],
        "своя",
    );
    assert!(writes("explicit-1") >= 1);
    assert_eq!(writes("hs-1"), w3);
    log(&[("CLAUDECODE", "1"), ("ZAM_SESSION", "-")], "без");
    assert_eq!(writes("hs-1"), w3);
}

/// Сервер выключен: пустой вывод, код 0, строка в `hooks.log`, время в пределе.
#[test]
fn fail_open_server_down_empty_exit_0() {
    let _g = serial();
    let p = Proj::new("fail-down", DEAD, "zam_x");
    let cases = [
        (
            "session-start",
            p.ev("SessionStart", "d1", json!({"source": "startup"})),
        ),
        (
            "prompt",
            p.ev(
                "UserPromptSubmit",
                "d1",
                json!({"prompt": "секретный промпт пользователя"}),
            ),
        ),
        (
            "post-tool",
            p.ev(
                "PostToolUseFailure",
                "d1",
                json!({"tool_name": "Bash", "error": "boom"}),
            ),
        ),
        (
            "checkpoint",
            p.ev("Stop", "d1", json!({"stop_hook_active": false})),
        ),
    ];
    for (k, v) in &cases {
        let (o, d) = p.hook_at(DEAD, k, "claude", v.to_string().as_bytes());
        empty(&o);
        assert!(d <= max_for(k), "{k}: {d:?}");
    }
    let log = p.log();
    assert!(
        log.contains("session-start") && log.contains("недоступен"),
        "{log}"
    );
    assert!(!log.contains("секретный промпт"), "{log}");
}

/// Мусор на stdin — тишина и код 0 у всех команд; неверные аргументы `zam hook` — тоже 0.
#[test]
fn fail_open_garbage_stdin() {
    let _g = serial();
    let p = Proj::new("fail-garbage", DEAD, "zam_x");
    for k in [
        "session-start",
        "prompt",
        "pre-tool",
        "post-tool",
        "checkpoint",
    ] {
        for stdin in [
            &b""[..],
            b"not json",
            b"[1,2,3]",
            b"{\"hook_event_name\": 5}",
            &[0xff, 0xfe, 0x00][..],
        ] {
            let (o, _) = p.hook_at(DEAD, k, "claude", stdin);
            empty(&o);
        }
    }
    let o = p.cmd(DEAD).args(["hook", "nope"]).output().unwrap();
    assert_eq!(o.status.code(), Some(0));
    let o = p
        .cmd(DEAD)
        .args(["hook", "pre-tool", "--agent", "nope"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0));
}

/// Неизвестное или чужое событие — тишина.
#[test]
fn fail_open_unknown_event() {
    let _g = serial();
    let p = Proj::new("fail-event", DEAD, "zam_x");
    write_cache(&p, DEAD);
    let mut v = write_ev(&p, "u1", "x.sh");
    v["hook_event_name"] = json!("SomethingNew");
    empty(
        &p.hook_at(DEAD, "pre-tool", "claude", v.to_string().as_bytes())
            .0,
    );
    // событие другой команды
    v["hook_event_name"] = json!("PostToolUse");
    empty(
        &p.hook_at(DEAD, "pre-tool", "claude", v.to_string().as_bytes())
            .0,
    );
    v["hook_event_name"] = json!("PreToolUse");
    empty(
        &p.hook_at(DEAD, "checkpoint", "claude", v.to_string().as_bytes())
            .0,
    );
}

/// Огромный `tool_input.content` (3 МБ): читается 1 МБ, поля спасаются, подсказка есть.
#[test]
fn huge_tool_input_truncated_read() {
    let _g = serial();
    let p = Proj::new("huge", DEAD, "zam_x");
    write_cache(&p, DEAD);
    // порядок полей — как у Claude Code: tool_name и file_path раньше content
    let f = p.repo.join("big/deploy.sh");
    let stdin = format!(
        "{{\"session_id\":\"h1\",\"transcript_path\":\"t\",\"cwd\":{},         \"hook_event_name\":\"PreToolUse\",\"tool_name\":\"Write\",         \"tool_input\":{{\"file_path\":{},\"content\":\"{}\"}}}}",
        json!(p.repo),
        json!(f),
        "x".repeat(3 * 1024 * 1024)
    );
    let (o, d) = p.hook_at(DEAD, "pre-tool", "claude", stdin.as_bytes());
    assert!(context(&o, "PreToolUse").contains("i:E1"));
    assert!(d <= OTHER_MAX, "{d:?}");
}

/// Битый кэш: `pre-tool` молчит и пишет в журнал, `session-start` переписывает файл.
#[test]
fn corrupt_cache_is_ignored_and_rewritten() {
    let _g = serial();
    let (url, tok) = start("corrupt");
    let p = Proj::new("corrupt", &url, &tok);
    std::fs::create_dir_all(p.cache()).unwrap();
    let path = p.cache().join("triggers.json");
    std::fs::write(&path, "{не json").unwrap();
    empty(&p.hook("pre-tool", &write_ev(&p, "c1", "x.sh")));
    assert!(p.log().contains("кэш триггеров битый"), "{}", p.log());
    p.hook(
        "session-start",
        &p.ev("SessionStart", "c1", json!({"source": "startup"})),
    );
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(v["etag"].as_str().is_some_and(|e| !e.is_empty()), "{v}");
    assert_eq!(v["triggers"], json!([]));
    // временных файлов не осталось
    let left: Vec<_> = std::fs::read_dir(p.cache())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(left.is_empty());
}

/// Вывод — JSON `hookSpecificOutput` с событием из stdin у обоих агентов; Codex — тот же формат.
#[test]
fn output_is_valid_hook_json_with_event_name() {
    let _g = serial();
    let (url, tok) = start("output");
    let p = Proj::new("output", &url, &tok);
    // ловушка — в дайджест старта
    p.insight("Скрипты — только LF", "gotcha", "global", &[]);
    for i in 0..3 {
        p.insight(
            &format!("Квазар и пульсар, заметка {i}"),
            "discovery",
            "global",
            &[],
        );
    }
    let ss = p.ev("SessionStart", "o1", json!({"source": "startup"}));
    for agent in ["claude", "codex"] {
        let (o, d) = p.hook_at(&url, "session-start", agent, ss.to_string().as_bytes());
        let s = ok(&o);
        assert_eq!(s.lines().count(), 1, "{s}");
        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v.as_object().unwrap().len(), 1, "{v}");
        assert_eq!(v["hookSpecificOutput"].as_object().unwrap().len(), 2, "{v}");
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
        assert!(d <= max_for("session-start"), "{agent}: {d:?}");
    }
    // prompt: подсказка (повтор в той же сессии сервер гасит — каждый раз новая сессия)
    let mut found = None;
    for i in 0..5 {
        let pr = p.ev(
            "UserPromptSubmit",
            &format!("op{i}"),
            json!({"prompt": "почему квазар светит"}),
        );
        let (o, d) = p.hook_at(&url, "prompt", "claude", pr.to_string().as_bytes());
        assert_eq!(o.status.code(), Some(0));
        if !out(&o).trim().is_empty() {
            found = Some((context(&o, "UserPromptSubmit"), d));
            break;
        }
    }
    let (c, d) = found.expect("prompt без подсказки за 5 попыток");
    assert!(c.contains("Квазар и пульсар"), "{c}");
    eprintln!("ZAM_TIMING prompt (тёплый) {d:?}");
}

/// R5: `[hooks]` проекта сильнее домашнего; выключено на сервере (кэш) — выключено.
#[test]
fn hooks_settings_disable_types() {
    let _g = serial();
    let p = Proj::new("settings", DEAD, "zam_x");
    write_cache(&p, DEAD);
    let pre = |s: &str| {
        p.hook_at(
            DEAD,
            "pre-tool",
            "claude",
            write_ev(&p, s, "x.sh").to_string().as_bytes(),
        )
        .0
    };
    std::fs::create_dir_all(p.home.join(".zeroaimemory")).unwrap();
    std::fs::write(
        p.home.join(".zeroaimemory/config.toml"),
        "url = \"http://x\"\n[hooks]\npre_tool = false\n",
    )
    .unwrap();
    empty(&pre("a"));
    std::fs::write(
        p.repo.join(".zeroaimemory/config.toml"),
        "[hooks]\npre_tool = true\n",
    )
    .unwrap();
    assert!(context(&pre("b"), "PreToolUse").contains("i:E1"));
    std::fs::write(
        p.cache().join("hooks.json"),
        json!({"fetched_at": "x", "server": DEAD, "hooks": {"pre_tool": false}}).to_string(),
    )
    .unwrap();
    empty(&pre("c"));
    // правка при выключенных подсказках всё равно отмечается для checkpoint
    assert!(p.session_file("edits", "c", "").is_some());
}

/// R3: время `session-start` (≤ 1200 мс), `prompt` и `post-tool` (≤ 1000 мс), `checkpoint`
/// (≤ 400 мс) с сервером — p95 из 20 запусков, и каждый ответ непустой (значения — в журнал
/// этапа).
#[test]
fn hooks_with_server_within_margin() {
    let _g = serial();
    let (url, tok) = start("timing");
    let p = Proj::new("timing", &url, &tok);
    p.insight(
        "CRLF в shebang",
        "failure",
        "global",
        &["error_regex:bad interpreter"],
    );
    // ловушка — в дайджест старта, «квазары» — подсказки к запросу
    p.insight("Скрипты — только LF", "gotcha", "global", &[]);
    for i in 0..3 {
        p.insight(
            &format!("Квазар и пульсар, заметка {i}"),
            "discovery",
            "global",
            &[],
        );
    }
    let mut report = Vec::new();
    for (k, mk) in [
        (
            "session-start",
            Box::new(|i: usize| {
                p.ev(
                    "SessionStart",
                    &format!("tm{i}"),
                    json!({"source": "startup"}),
                )
            }) as Box<dyn Fn(usize) -> Value>,
        ),
        (
            "prompt",
            Box::new(|i| {
                p.ev(
                    "UserPromptSubmit",
                    &format!("tm{i}"),
                    json!({"prompt": "почему квазар светит"}),
                )
            }),
        ),
        (
            "post-tool",
            Box::new(|i| {
                p.ev(
                    "PostToolUseFailure",
                    &format!("tm{i}"),
                    json!({"tool_name": "Bash", "error": "/bin/bash^M: bad interpreter"}),
                )
            }),
        ),
        (
            "checkpoint",
            Box::new(|i| {
                p.ev(
                    "Stop",
                    &format!("tm{i}"),
                    json!({"stop_hook_active": false}),
                )
            }),
        ),
    ] {
        let mut times = Vec::new();
        for i in 0..20 {
            if k == "checkpoint" {
                p.hook("pre-tool", &write_ev(&p, &format!("tm{i}"), "a.txt"));
            }
            let (o, d) = p.hook_at(&url, k, "claude", mk(i).to_string().as_bytes());
            assert!(!ok(&o).trim().is_empty(), "{k} #{i}: пустой ответ");
            times.push(d);
        }
        let q = p95(times);
        report.push(format!("{k} p95={q:?}"));
        assert!(q <= max_for(k), "{k}: p95 {q:?}");
    }
    eprintln!("ZAM_TIMING {}", report.join(", "));
}

// ---------- M4.5, задача 8: журнал сессии, знание о файле, очередь изменённых файлов ----------

/// Индекс `file-refs.json` вручную: `src/pool.rs` — ловушка (`ok`) и заметка (`stale`).
fn write_file_refs(p: &Proj, server: &str) {
    let c = json!({
        "etag": "f1", "fetched_at": "2026-10-29T00:00:00Z", "server": server, "solution": G,
        "refs": [
            {"path": "src/pool.rs", "hash": "a".repeat(64), "items": [
                {"id": "i:E7", "kind": "gotcha", "title": "Пул соединений течёт", "state": "ok"},
                {"id": "n:N1", "kind": "note", "title": "Архитектура пула", "state": "stale"},
            ]},
            {"path": "scripts/deploy.sh", "items": [
                {"id": "c:C1", "kind": "changelog", "title": "Деплой через systemd", "state": "ok"},
            ]},
        ],
    });
    std::fs::create_dir_all(p.cache()).unwrap();
    std::fs::write(p.cache().join("file-refs.json"), c.to_string()).unwrap();
}

/// Строка знания о `src/pool.rs` из [`write_file_refs`].
const POOL_LINE: &str = "ZeroAIMemory: о файле src/pool.rs в памяти: Пул соединений течёт [gotcha] \
— i:E7; Архитектура пула [note, возможно устарело] — n:N1";

/// Claude `PostToolUse` (успех) инструмента `tool` с `tool_input`.
fn post_ok(p: &Proj, session: &str, tool: &str, input: Value) -> Value {
    p.ev(
        "PostToolUse",
        session,
        json!({"tool_name": tool, "tool_use_id": "u9", "tool_input": input,
               "tool_response": {"success": true}}),
    )
}

/// События журнала сессии (`events-<сессия>.jsonl`), строки JSON.
fn events(p: &Proj, session: &str) -> Vec<Value> {
    let Some(f) = p.session_file("events", session, ".jsonl") else {
        return Vec::new();
    };
    std::fs::read_to_string(f)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// Очередь изменённых файлов сессии (`changed-<сессия>.jsonl`).
fn queue(p: &Proj, session: &str) -> Vec<Value> {
    let Some(f) = p.session_file("changed", session, ".jsonl") else {
        return Vec::new();
    };
    std::fs::read_to_string(f)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn blake(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

#[test]
fn post_tool_read_shows_file_knowledge_offline() {
    let _g = serial();
    let p = Proj::new("post-read", DEAD, "zam_x");
    write_file_refs(&p, DEAD);
    let read = post_ok(
        &p,
        "r1",
        "Read",
        json!({"file_path": p.repo.join("src").join("pool.rs")}),
    );
    let (o, _) = p.hook_at(DEAD, "post-tool", "claude", read.to_string().as_bytes());
    assert_eq!(context(&o, "PostToolUse"), POOL_LINE);
    // путь относительно cwd и с `./` — тот же файл (другая сессия)
    let rel = post_ok(
        &p,
        "r2",
        "Read",
        json!({"file_path": "./src/../src/pool.rs"}),
    );
    let (o, _) = p.hook_at(DEAD, "post-tool", "claude", rel.to_string().as_bytes());
    assert_eq!(context(&o, "PostToolUse"), POOL_LINE);
    // файл, о котором память ничего не знает, — тишина; чтение записано в журнал
    let other = post_ok(
        &p,
        "r1",
        "Read",
        json!({"file_path": p.repo.join("README.md")}),
    );
    empty(
        &p.hook_at(DEAD, "post-tool", "claude", other.to_string().as_bytes())
            .0,
    );
    let ev = events(&p, "r1");
    assert_eq!(ev.len(), 2, "{ev:?}");
    assert_eq!(ev[0]["ev"], "read");
    assert_eq!(ev[0]["path"], "src/pool.rs");
    assert_eq!(ev[1]["path"], "README.md");
    assert!(ev[0]["ts"].as_u64().is_some());
    // кэш другого сервера не используется
    let s2 = post_ok(&p, "r3", "Read", json!({"file_path": "src/pool.rs"}));
    empty(
        &p.hook_at(
            "http://127.0.0.1:10",
            "post-tool",
            "claude",
            s2.to_string().as_bytes(),
        )
        .0,
    );
    // ошибки хука в журнале нет
    assert_eq!(p.log(), "", "hooks.log");
}

#[test]
fn post_tool_read_once_per_file() {
    let _g = serial();
    let p = Proj::new("post-read-once", DEAD, "zam_x");
    write_file_refs(&p, DEAD);
    let read = |s: &str, f: &str| {
        let v = post_ok(&p, s, "Read", json!({"file_path": p.repo.join(f)}));
        p.hook_at(DEAD, "post-tool", "claude", v.to_string().as_bytes())
            .0
    };
    assert_eq!(
        context(&read("o1", "src/pool.rs"), "PostToolUse"),
        POOL_LINE
    );
    empty(&read("o1", "src/pool.rs"));
    // другой файл в той же сессии — показывается; правка уже показанного — без строки о файле
    assert!(context(&read("o1", "scripts/deploy.sh"), "PostToolUse").contains("c:C1"));
    let pre = p.ev(
        "PreToolUse",
        "o1",
        json!({"tool_name": "Edit", "tool_input": {"file_path": p.repo.join("src/pool.rs")}}),
    );
    empty(
        &p.hook_at(DEAD, "pre-tool", "claude", pre.to_string().as_bytes())
            .0,
    );
    // новая сессия — снова; pre-tool на правку показывает знание раньше триггеров file_glob
    assert_eq!(
        context(&read("o2", "src/pool.rs"), "PostToolUse"),
        POOL_LINE
    );
    write_cache(&p, DEAD);
    let pre = p.ev(
        "PreToolUse",
        "o3",
        json!({"tool_name": "Write",
               "tool_input": {"file_path": p.repo.join("scripts/deploy.sh"), "content": "x"}}),
    );
    let c = context(
        &p.hook_at(DEAD, "pre-tool", "claude", pre.to_string().as_bytes())
            .0,
        "PreToolUse",
    );
    let lines: Vec<&str> = c.lines().collect();
    assert_eq!(lines.len(), 2, "{c}");
    assert!(lines[0].starts_with("ZeroAIMemory: о файле scripts/deploy.sh в памяти: Деплой через systemd [changelog] — c:C1"), "{c}");
    assert!(lines[1].contains("для **/*.sh"), "{c}");
    // показанное — ключом file:<путь> в shown-файле сессии
    let shown = std::fs::read_to_string(p.session_file("shown", "o1", ".json").unwrap()).unwrap();
    assert!(shown.contains("file:src/pool.rs"), "{shown}");
}

#[test]
fn post_tool_edit_queues_hash() {
    let _g = serial();
    let p = Proj::new("post-edit", DEAD, "zam_x");
    std::fs::create_dir_all(p.repo.join("src")).unwrap();
    std::fs::write(p.repo.join("src/a.rs"), b"fn a() {}\n").unwrap();
    std::fs::write(p.repo.join("src/b.rs"), b"fn b() {}\n").unwrap();
    for (tool, input) in [
        (
            "Edit",
            json!({"file_path": p.repo.join("src/a.rs"), "old_string": "a", "new_string": "b"}),
        ),
        (
            "Write",
            json!({"file_path": p.repo.join("src/b.rs"), "content": "fn b() {}\n"}),
        ),
        (
            "MultiEdit",
            json!({"file_path": p.repo.join("src/a.rs"), "edits": []}),
        ),
        (
            "NotebookEdit",
            json!({"notebook_path": p.repo.join("gone.ipynb")}),
        ),
    ] {
        let v = post_ok(&p, "e1", tool, input);
        empty(
            &p.hook_at(DEAD, "post-tool", "claude", v.to_string().as_bytes())
                .0,
        );
    }
    let q = queue(&p, "e1");
    assert_eq!(
        q,
        vec![
            json!({"path": "src/a.rs", "hash": blake(b"fn a() {}\n")}),
            json!({"path": "src/b.rs", "hash": blake(b"fn b() {}\n")}),
            json!({"path": "src/a.rs", "hash": blake(b"fn a() {}\n")}),
            // удалённый файл — «хэш» нулей
            json!({"path": "gone.ipynb", "hash": "0".repeat(64)}),
        ]
    );
    let ev = events(&p, "e1");
    let edits: Vec<&str> = ev
        .iter()
        .filter(|e| e["ev"] == "edit")
        .map(|e| e["path"].as_str().unwrap())
        .collect();
    assert_eq!(edits, ["src/a.rs", "src/b.rs", "src/a.rs", "gone.ipynb"]);
    // правка отмечена для checkpoint; Bash — событие cmd без аргументов
    assert!(p.session_file("edits", "e1", "").is_some());
    let bash = post_ok(
        &p,
        "e1",
        "Bash",
        json!({"command": "cargo test --secret-flag"}),
    );
    empty(
        &p.hook_at(DEAD, "post-tool", "claude", bash.to_string().as_bytes())
            .0,
    );
    let last = events(&p, "e1").pop().unwrap();
    assert_eq!(last["ev"], "cmd");
    assert_eq!(last["exe"], "cargo");
    assert_eq!(last["ok"], true);
    // файл вне корня — ни события, ни очереди
    let out = post_ok(
        &p,
        "e2",
        "Edit",
        json!({"file_path": p.repo.parent().unwrap().join("x.rs")}),
    );
    empty(
        &p.hook_at(DEAD, "post-tool", "claude", out.to_string().as_bytes())
            .0,
    );
    assert!(queue(&p, "e2").is_empty());
    assert!(events(&p, "e2").is_empty());
    // ошибка инструмента — error и cmd {ok: false} с отпечатком
    let fail = p.ev(
        "PostToolUseFailure",
        "e3",
        json!({"tool_name": "Bash", "tool_input": {"command": "./build.sh --token abc"},
               "error": "/bin/bash^M: bad interpreter at line 12"}),
    );
    let _ = p.hook_at(DEAD, "post-tool", "claude", fail.to_string().as_bytes());
    let ev = events(&p, "e3");
    assert_eq!(ev.len(), 2, "{ev:?}");
    assert_eq!(ev[0]["ev"], "error");
    assert_eq!(ev[0]["tool"], "Bash");
    assert_eq!(ev[1]["ev"], "cmd");
    assert_eq!(ev[1]["exe"], "build.sh");
    assert_eq!(ev[1]["ok"], false);
    assert_eq!(ev[0]["err_fp"], ev[1]["err_fp"]);
    assert_eq!(ev[0]["err_fp"].as_str().unwrap().len(), 16);
    assert!(!ev[0]["text"].as_str().unwrap().contains("12"), "{ev:?}");
    let raw = std::fs::read_to_string(p.session_file("events", "e3", ".jsonl").unwrap()).unwrap();
    assert!(
        !raw.contains("abc"),
        "аргументы команды не в журнале: {raw}"
    );
}

/// Codex: `PostToolUse` `apply_patch` без stderr — успех: правка каждого файла патча в журнал
/// и очередь; со stderr — режим ошибки.
#[test]
fn codex_apply_patch_edit_events() {
    let _g = serial();
    let p = Proj::new("codex-patch-post", DEAD, "zam_x");
    std::fs::create_dir_all(p.repo.join("src")).unwrap();
    std::fs::write(p.repo.join("src/main.rs"), b"fn main() {}\n").unwrap();
    std::fs::write(p.repo.join("new.txt"), b"new\n").unwrap();
    let patch = "*** Begin Patch\n*** Update File: src/main.rs\n@@\n-a\n+b\n*** Add File: new.txt\n+new\n*** Delete File: old.txt\n*** End Patch\n";
    let ev = p.ev(
        "PostToolUse",
        "cx",
        json!({"tool_name": "apply_patch", "tool_input": {"command": patch},
               "tool_response": "{\"stdout\":\"Success. Updated the following files\"}"}),
    );
    empty(
        &p.hook_at(DEAD, "post-tool", "codex", ev.to_string().as_bytes())
            .0,
    );
    let edits: Vec<String> = events(&p, "cx")
        .iter()
        .filter(|e| e["ev"] == "edit")
        .map(|e| e["path"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(edits, ["src/main.rs", "new.txt", "old.txt"]);
    let q = queue(&p, "cx");
    assert_eq!(q.len(), 3);
    assert_eq!(q[0]["hash"], blake(b"fn main() {}\n"));
    assert_eq!(q[2]["hash"], "0".repeat(64));
    // Bash Codex со stderr — ошибка, без строки в очереди
    let bad = p.ev(
        "PostToolUse",
        "cx",
        json!({"tool_name": "Bash", "tool_input": {"command": "make"},
               "tool_response": {"stdout": "", "stderr": "make: *** No rule"}}),
    );
    let _ = p.hook_at(DEAD, "post-tool", "codex", bad.to_string().as_bytes());
    let last = events(&p, "cx").pop().unwrap();
    assert_eq!(
        (last["ev"].as_str(), last["ok"].as_bool()),
        (Some("cmd"), Some(false))
    );
    assert_eq!(queue(&p, "cx").len(), 3);
}

/// Успешный `post-tool` без сети — 100 запусков (чтение со знанием о файле и журналом):
/// p95 ≤ 200 мс (×4 от бюджета 50 мс, как `pre_tool_under_50ms`); значения — в журнал этапа.
#[test]
fn post_tool_success_under_50ms() {
    let _g = serial();
    let p = Proj::new("post-timing", DEAD, "zam_x");
    write_file_refs(&p, DEAD);
    std::fs::create_dir_all(p.repo.join("src")).unwrap();
    std::fs::write(p.repo.join("src/pool.rs"), "x".repeat(64 * 1024)).unwrap();
    let mut times = Vec::new();
    for i in 0..100 {
        let (tool, input) = if i % 2 == 0 {
            ("Read", json!({"file_path": p.repo.join("src/pool.rs")}))
        } else {
            (
                "Edit",
                json!({"file_path": p.repo.join("src/pool.rs"), "old_string": "x"}),
            )
        };
        let v = post_ok(&p, &format!("pt{i}"), tool, input);
        let (o, d) = p.hook_at(DEAD, "post-tool", "claude", v.to_string().as_bytes());
        if tool == "Read" {
            assert_eq!(context(&o, "PostToolUse"), POOL_LINE);
        } else {
            empty(&o);
        }
        times.push(d);
    }
    let q = p95(times.clone());
    let min = times.iter().min().unwrap();
    eprintln!("ZAM_TIMING post-tool (успех) p95={q:?} min={min:?} (100 запусков)");
    assert!(q <= PRE_TOOL_P95_MAX, "post-tool p95 {q:?}");
    assert_eq!(p.log(), "", "hooks.log");
}

/// Выключенный `file_context` (клиент или сервер) — без строк о файле; журнал и очередь
/// остаются; выключенный `session_journal` — журнала нет.
#[test]
fn file_context_disabled_silent() {
    let _g = serial();
    let p = Proj::new("file-ctx-off", DEAD, "zam_x");
    write_file_refs(&p, DEAD);
    write_cache(&p, DEAD);
    std::fs::write(
        p.repo.join(".zeroaimemory/config.toml"),
        "[hooks]\nfile_context = false\n",
    )
    .unwrap();
    let read = post_ok(
        &p,
        "f1",
        "Read",
        json!({"file_path": p.repo.join("src/pool.rs")}),
    );
    empty(
        &p.hook_at(DEAD, "post-tool", "claude", read.to_string().as_bytes())
            .0,
    );
    assert_eq!(events(&p, "f1").len(), 1, "журнал ведётся");
    // pre-tool: знания о файле нет, триггер file_glob — есть
    let pre = p.ev(
        "PreToolUse",
        "f1",
        json!({"tool_name": "Write",
               "tool_input": {"file_path": p.repo.join("scripts/deploy.sh"), "content": "x"}}),
    );
    let c = context(
        &p.hook_at(DEAD, "pre-tool", "claude", pre.to_string().as_bytes())
            .0,
        "PreToolUse",
    );
    assert!(!c.contains("о файле"), "{c}");
    assert!(c.contains("i:E1"), "{c}");
    // выключено на сервере (hooks.json), в клиенте включено
    std::fs::write(
        p.repo.join(".zeroaimemory/config.toml"),
        "[hooks]\nfile_context = true\nsession_journal = false\n",
    )
    .unwrap();
    std::fs::write(
        p.cache().join("hooks.json"),
        json!({"fetched_at": "x", "server": DEAD, "hooks": {"file_context": false}}).to_string(),
    )
    .unwrap();
    let read = post_ok(
        &p,
        "f2",
        "Read",
        json!({"file_path": p.repo.join("src/pool.rs")}),
    );
    empty(
        &p.hook_at(DEAD, "post-tool", "claude", read.to_string().as_bytes())
            .0,
    );
    assert!(events(&p, "f2").is_empty(), "session_journal = false");
    let prompt = p.ev("UserPromptSubmit", "f2", json!({"prompt": "секрет"}));
    empty(
        &p.hook_at(DEAD, "prompt", "claude", prompt.to_string().as_bytes())
            .0,
    );
    assert!(events(&p, "f2").is_empty());
}

/// `hooks.json` старого сервера (пять ключей M4) и `config.toml` без новых ключей — новые
/// типы включены.
#[test]
fn old_hooks_json_without_new_keys_enables() {
    let _g = serial();
    let p = Proj::new("old-hooks-json", DEAD, "zam_x");
    write_file_refs(&p, DEAD);
    std::fs::write(
        p.cache().join("hooks.json"),
        json!({"fetched_at": "2026-10-27T00:00:00Z", "server": DEAD,
               "hooks": {"session_start": true, "prompt": true, "pre_tool": true,
                         "post_tool": true, "checkpoint": true}})
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        p.repo.join(".zeroaimemory/config.toml"),
        "[hooks]\nprompt = true\n",
    )
    .unwrap();
    let read = post_ok(
        &p,
        "h1",
        "Read",
        json!({"file_path": p.repo.join("src/pool.rs")}),
    );
    let (o, _) = p.hook_at(DEAD, "post-tool", "claude", read.to_string().as_bytes());
    assert_eq!(context(&o, "PostToolUse"), POOL_LINE);
    let prompt = p.ev(
        "UserPromptSubmit",
        "h1",
        json!({"prompt": "почему пул течёт"}),
    );
    let _ = p.hook_at(DEAD, "prompt", "claude", prompt.to_string().as_bytes());
    let ev = events(&p, "h1");
    assert_eq!(ev.len(), 2, "{ev:?}");
    assert_eq!(ev[1], json!({"ts": ev[1]["ts"], "ev": "prompt", "len": 16}));
}

/// `GET /file-refs` с решением: записи со ссылками (`zam log … --ref`).
fn file_refs_index(p: &Proj) -> Value {
    let r = http()
        .get(&format!("{}/api/v1/file-refs?solution={G}", p.url))
        .header("Authorization", format!("Bearer {}", p.token))
        .call()
        .unwrap();
    serde_json::from_str(&r.into_body().read_to_string().unwrap()).unwrap()
}

fn ref_state(idx: &Value, path: &str) -> Vec<String> {
    idx["refs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["path"] == path)
        .map(|r| {
            r["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| i["state"].as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Правка файла уходит очередью `POST /files/changed` из `prompt`: ссылка `stale`, очередь
/// пуста; `session-start` кэширует индекс `file-refs.json` (ETag), `Read` показывает знание.
#[test]
fn prompt_flushes_changed_queue() {
    let _g = serial();
    let (url, tok) = start("flush");
    let p = Proj::new("flush", &url, &tok);
    std::fs::create_dir_all(p.repo.join("src")).unwrap();
    std::fs::write(p.repo.join("src/pool.rs"), b"v1\n").unwrap();
    let o = p.zam(
        &[
            "log",
            "insight",
            "--kind",
            "gotcha",
            "--title",
            "Пул соединений течёт",
            "--ref",
            "src/pool.rs",
        ],
        "Разобрано.\n".as_bytes(),
    );
    let id = id_of(&ok(&o));
    assert_eq!(ref_state(&file_refs_index(&p), "src/pool.rs"), ["ok"]);
    // session-start: индекс в кэш
    let ss = p.ev("SessionStart", "fl", json!({"source": "startup"}));
    let _ = p.hook_at(&url, "session-start", "claude", ss.to_string().as_bytes());
    let cache: Value =
        serde_json::from_str(&std::fs::read_to_string(p.cache().join("file-refs.json")).unwrap())
            .unwrap();
    assert_eq!(cache["solution"], G);
    assert!(!cache["etag"].as_str().unwrap().is_empty());
    assert_eq!(cache["refs"][0]["items"][0]["id"], format!("i:{id}"));
    let read = post_ok(
        &p,
        "fl",
        "Read",
        json!({"file_path": p.repo.join("src/pool.rs")}),
    );
    let (o, _) = p.hook_at(&url, "post-tool", "claude", read.to_string().as_bytes());
    assert!(
        context(&o, "PostToolUse").contains(&format!("Пул соединений течёт [insight] — i:{id}"))
    );
    // правка → очередь → prompt отправляет
    std::fs::write(p.repo.join("src/pool.rs"), b"v2\n").unwrap();
    let edit = post_ok(
        &p,
        "fl",
        "Edit",
        json!({"file_path": p.repo.join("src/pool.rs")}),
    );
    let _ = p.hook_at(&url, "post-tool", "claude", edit.to_string().as_bytes());
    assert_eq!(queue(&p, "fl").len(), 1);
    let pr = p.ev("UserPromptSubmit", "fl", json!({"prompt": "что с пулом"}));
    let _ = p.hook_at(&url, "prompt", "claude", pr.to_string().as_bytes());
    assert!(queue(&p, "fl").is_empty(), "очередь отправлена");
    assert_eq!(ref_state(&file_refs_index(&p), "src/pool.rs"), ["stale"]);
    // сервер недоступен — очередь остаётся до следующего раза
    std::fs::write(p.repo.join("src/pool.rs"), b"v1\n").unwrap();
    let _ = p.hook_at(&url, "post-tool", "claude", edit.to_string().as_bytes());
    let _ = p.hook_at(DEAD, "prompt", "claude", pr.to_string().as_bytes());
    assert_eq!(queue(&p, "fl").len(), 1, "ошибка — очередь цела");
    let stop = p.ev("Stop", "fl", json!({"stop_hook_active": false}));
    let _ = p.hook_at(&url, "checkpoint", "claude", stop.to_string().as_bytes());
    assert!(queue(&p, "fl").is_empty(), "checkpoint отправил");
    assert_eq!(
        ref_state(&file_refs_index(&p), "src/pool.rs"),
        ["ok"],
        "хэш вернулся"
    );
    let ev = events(&p, "fl");
    assert!(ev.iter().any(|e| e["ev"] == "prompt"), "{ev:?}");
}

/// `memory_write` из `zam` агента — только в журнал, который уже завели хуки этой сессии;
/// без журнала команда ничего не создаёт.
#[test]
fn cli_memory_write_only_into_existing_journal() {
    let _g = serial();
    let (url, tok) = start("memwrite");
    let p = Proj::new("memwrite", &url, &tok);
    let log = |session: &str| {
        let o = p
            .cmd(&url)
            .env("ZAM_SESSION", session)
            .args(["log", "changelog", "--kind", "fix", "--title", "Правка"])
            .write_stdin(b"x\n".to_vec())
            .output()
            .unwrap();
        ok(&o);
    };
    log("mw-none");
    assert!(p.session_file("events", "mw-none", ".jsonl").is_none());
    let pr = p.ev("UserPromptSubmit", "mw1", json!({"prompt": "п"}));
    let _ = p.hook_at(DEAD, "prompt", "claude", pr.to_string().as_bytes());
    log("mw1");
    let ev = events(&p, "mw1");
    assert_eq!(ev.last().unwrap()["ev"], "memory_write", "{ev:?}");
    assert_eq!(ev.last().unwrap()["kind"], "changelog");
}

// ---------- fix round 1 задачи 8 ----------

/// I-1: успешный Bash у Claude Code со stderr (предупреждения, прогресс) — успех: `cmd {ok:
/// true}`, ни `error`, ни запроса к серверу.
#[test]
fn claude_bash_stderr_on_success_is_not_error() {
    let _g = serial();
    let (url, seen) = common::stub(200, "{}", Duration::ZERO);
    let p = Proj::new("claude-stderr", &url, "zam_x");
    let ev = p.ev(
        "PostToolUse",
        "cs",
        json!({"tool_name": "Bash", "tool_input": {"command": "cargo build"},
               "tool_response": {"stdout": "", "stderr": "warning: unused variable",
                                 "interrupted": false}}),
    );
    empty(
        &p.hook_at(&url, "post-tool", "claude", ev.to_string().as_bytes())
            .0,
    );
    let ev = events(&p, "cs");
    assert_eq!(ev.len(), 1, "{ev:?}");
    assert_eq!(
        (ev[0]["ev"].as_str(), ev[0]["ok"].as_bool()),
        (Some("cmd"), Some(true))
    );
    assert!(
        seen.lock().unwrap().is_empty(),
        "{:?}",
        seen.lock().unwrap()
    );
    // Codex с кодом выхода 0 — тоже успех
    let cx = p.ev(
        "PostToolUse",
        "cs",
        json!({"tool_name": "Bash", "tool_input": {"command": "make"},
               "tool_response": {"stdout": "", "stderr": "make: progress", "exit_code": 0}}),
    );
    empty(
        &p.hook_at(&url, "post-tool", "codex", cx.to_string().as_bytes())
            .0,
    );
    assert_eq!(events(&p, "cs").last().unwrap()["ok"], true);
    assert!(seen.lock().unwrap().is_empty());
}

/// Очередь `changed-<сессия>.jsonl` вручную (имя — как у хуков: очищенный id и 8 hex blake3).
fn put_queue(p: &Proj, session: &str, files: &[(String, char)]) {
    std::fs::create_dir_all(p.cache()).unwrap();
    let mut s = String::new();
    for (path, h) in files {
        s.push_str(&json!({"path": path, "hash": h.to_string().repeat(64)}).to_string());
        s.push('\n');
    }
    let part = format!("{session}-{}", &blake(session.as_bytes())[..8]);
    std::fs::write(p.cache().join(format!("changed-{part}.jsonl")), s).unwrap();
}

/// I-2: пять очередей — один `POST /files/changed` (повтор пути — последний хэш), очереди
/// очищены; медленный сервер — `session-start` укладывается в бюджет, очереди целы.
#[test]
fn session_start_flushes_all_queues_in_one_request() {
    let _g = serial();
    let (url, seen) = common::stub(200, r#"{"stale":0,"text":""}"#, Duration::ZERO);
    let p = Proj::new("flush-one", &url, "zam_x");
    let hashes = ['a', 'b', 'c', 'd', 'e'];
    for (i, h) in hashes.iter().enumerate() {
        put_queue(
            &p,
            &format!("q{i}"),
            &[("src/common.rs".into(), *h), (format!("src/f{i}.rs"), 'f')],
        );
    }
    let ss = p.ev("SessionStart", "q-new", json!({"source": "startup"}));
    let _ = p.hook_at(&url, "session-start", "claude", ss.to_string().as_bytes());
    let s = seen.lock().unwrap().clone();
    let posts: Vec<&Value> = s
        .iter()
        .filter(|(m, path, _)| m == "POST" && path == "/api/v1/files/changed")
        .map(|(_, _, b)| b)
        .collect();
    assert_eq!(posts.len(), 1, "{s:?}");
    let files = posts[0]["files"].as_array().unwrap();
    assert_eq!(files.len(), 6, "{files:?}");
    assert_eq!(
        files
            .iter()
            .filter(|f| f["path"] == "src/common.rs")
            .count(),
        1
    );
    for i in 0..5 {
        assert!(
            queue(&p, &format!("q{i}")).is_empty(),
            "очередь q{i} отправлена"
        );
    }
    // медленный сервер: в бюджете, очереди возвращены
    let (slow, slow_seen) = common::stub(200, r#"{"stale":0}"#, Duration::from_secs(3));
    let p = Proj::new("flush-slow", &slow, "zam_x");
    for i in 0..5 {
        put_queue(&p, &format!("s{i}"), &[(format!("src/f{i}.rs"), 'b')]);
    }
    let ss = p.ev("SessionStart", "s-new", json!({"source": "startup"}));
    let (_, d) = p.hook_at(&slow, "session-start", "claude", ss.to_string().as_bytes());
    assert!(d <= max_for("session-start"), "session-start {d:?}");
    let posts = slow_seen
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, path, _)| path == "/api/v1/files/changed")
        .count();
    assert!(posts <= 1, "не больше одного запроса: {posts}");
    let left: usize = (0..5).map(|i| queue(&p, &format!("s{i}")).len()).sum();
    assert_eq!(left, 5, "тайм-аут — очереди целы");
}

/// M-6: постоянная ошибка 4xx — очередь отбрасывается со строкой в `hooks.log`; 429 — остаётся.
#[test]
fn changed_queue_dropped_on_permanent_4xx() {
    let _g = serial();
    let (url, _) = common::stub(
        422,
        r#"{"type":"/problems/unprocessable","status":422,"detail":"files[0]: путь"}"#,
        Duration::ZERO,
    );
    let p = Proj::new("flush-422", &url, "zam_x");
    put_queue(&p, "d1", &[("src/a.rs".into(), 'a')]);
    let pr = p.ev("UserPromptSubmit", "d1", json!({"prompt": "x"}));
    let _ = p.hook_at(&url, "prompt", "claude", pr.to_string().as_bytes());
    assert!(queue(&p, "d1").is_empty());
    assert!(p.log().contains("отброшена"), "{}", p.log());
    let (url, _) = common::stub(
        429,
        r#"{"status":429,"detail":"медленнее"}"#,
        Duration::ZERO,
    );
    let p = Proj::new("flush-429", &url, "zam_x");
    put_queue(&p, "d2", &[("src/a.rs".into(), 'a')]);
    let pr = p.ev("UserPromptSubmit", "d2", json!({"prompt": "x"}));
    let _ = p.hook_at(&url, "prompt", "claude", pr.to_string().as_bytes());
    assert_eq!(queue(&p, "d2").len(), 1, "429 — повтор позже");
    // задача 9: 403, 404, 410 бывают временными (доступ не одобрен, старый сервер) — очередь
    // остаётся; 400 и 413 — отбрасывается
    for (code, keep) in [
        (403, true),
        (404, true),
        (410, true),
        (400, false),
        (413, false),
    ] {
        let (url, _) = common::stub(
            code,
            Box::leak(format!(r#"{{"status":{code},"detail":"x"}}"#).into_boxed_str()),
            Duration::ZERO,
        );
        let p = Proj::new(&format!("flush-{code}"), &url, "zam_x");
        put_queue(&p, "d3", &[("src/a.rs".into(), 'a')]);
        let pr = p.ev("UserPromptSubmit", "d3", json!({"prompt": "x"}));
        let _ = p.hook_at(&url, "prompt", "claude", pr.to_string().as_bytes());
        assert_eq!(queue(&p, "d3").len(), usize::from(keep), "{code}");
    }
}

/// M-8: отказ `apply_patch` (Codex, stderr) ставит пути патча в очередь — патч мог
/// примениться частично.
#[test]
fn failed_apply_patch_still_queues_paths() {
    let _g = serial();
    let p = Proj::new("patch-fail", DEAD, "zam_x");
    std::fs::create_dir_all(p.repo.join("src")).unwrap();
    std::fs::write(p.repo.join("src/a.rs"), b"half\n").unwrap();
    let patch = "*** Begin Patch\n*** Update File: src/a.rs\n@@\n-a\n+b\n\
                 *** Update File: src/b.rs\n*** End Patch\n";
    let ev = p.ev(
        "PostToolUse",
        "pf",
        json!({"tool_name": "apply_patch", "tool_input": {"command": patch},
               "tool_response": {"stdout": "", "stderr": "error: hunk failed", "exit_code": 1}}),
    );
    let _ = p.hook_at(DEAD, "post-tool", "codex", ev.to_string().as_bytes());
    let q = queue(&p, "pf");
    assert_eq!(q.len(), 2, "{q:?}");
    assert_eq!(q[0], json!({"path": "src/a.rs", "hash": blake(b"half\n")}));
    assert_eq!(q[1]["hash"], "0".repeat(64));
    let ev = events(&p, "pf");
    assert_eq!(ev[0]["ev"], "error", "{ev:?}");
}

/// Ошибка Bash (`PostToolUseFailure`) командой `command` с текстом `error`.
fn bash_fail(p: &Proj, session: &str, command: &str, error: &str) -> Value {
    p.ev(
        "PostToolUseFailure",
        session,
        json!({"tool_name": "Bash", "tool_input": {"command": command}, "error": error}),
    )
}

/// Пара «ошибка → исправление» в журнале сессии: `cargo build` с ошибкой, правка `src/a.rs`,
/// `cargo build` без ошибки (сеть хукам не нужна — `DEAD`).
fn fix_pair(p: &Proj, session: &str) {
    std::fs::create_dir_all(p.repo.join("src")).unwrap();
    std::fs::write(p.repo.join("src/a.rs"), b"fn main() {}\n").unwrap();
    let steps = [
        bash_fail(
            p,
            session,
            "cargo build",
            "error[E0425]: cannot find value `x` in this scope",
        ),
        post_ok(
            p,
            session,
            "Edit",
            json!({"file_path": p.repo.join("src/a.rs")}),
        ),
        post_ok(p, session, "Bash", json!({"command": "cargo build"})),
    ];
    for s in steps {
        let _ = p.hook_at(DEAD, "post-tool", "claude", s.to_string().as_bytes());
    }
}

/// §9.2: `session-start` после `compact` (и `resume`) дописывает сводку журнала после текста
/// сервера; `startup` — без сводки; сервер недоступен — только сводка; журнала нет — только
/// сервер. Ключ `recovery` выключен в клиенте — без сводки.
#[test]
fn session_start_compact_appends_summary() {
    let _g = serial();
    let (url, tok) = start("compact-summary");
    let p = Proj::new("compact-summary", &url, &tok);
    p.insight("Пул соединений течёт", "gotcha", "global", &[]);
    let ss = |s: &str, src: &str| p.ev("SessionStart", s, json!({"source": src}));
    let _ = p.hook("session-start", &ss("cs", "startup"));
    fix_pair(&p, "cs");
    let c = context(
        &p.hook("session-start", &ss("cs", "compact")),
        "SessionStart",
    );
    let at = c
        .find("ZeroAIMemory: До сжатия в этой сессии: правились src/a.rs; ошибки: 1 (последняя: ")
        .unwrap_or_else(|| panic!("нет сводки: {c}"));
    assert!(
        c[..at].contains("Пул соединений течёт"),
        "сервер — первым: {c}"
    );
    assert!(
        c[at..].contains("исправлены: ")
            && c[at..].contains("cannot find value")
            && c[at..].contains("→ src/a.rs; записей в память: 0"),
        "{c}"
    );
    assert!(c.ends_with("записей в память: 0"), "{c}");
    let c = context(
        &p.hook("session-start", &ss("cs", "resume")),
        "SessionStart",
    );
    assert!(c.contains("До сжатия в этой сессии"), "{c}");
    let c = context(
        &p.hook("session-start", &ss("cs", "startup")),
        "SessionStart",
    );
    assert!(!c.contains("До сжатия"), "{c}");
    // сервер недоступен — только сводка
    let (o, _) = p.hook_at(
        DEAD,
        "session-start",
        "claude",
        ss("cs", "compact").to_string().as_bytes(),
    );
    let c = context(&o, "SessionStart");
    assert!(
        c.starts_with("ZeroAIMemory: До сжатия в этой сессии"),
        "{c}"
    );
    // журнала нет — только сервер
    let c = context(
        &p.hook("session-start", &ss("cs-new", "compact")),
        "SessionStart",
    );
    assert!(
        c.contains("Пул соединений") && !c.contains("До сжатия"),
        "{c}"
    );
    // `recovery` выключен в клиенте
    std::fs::write(
        p.repo.join(".zeroaimemory/config.toml"),
        "[hooks]\nrecovery = false\n",
    )
    .unwrap();
    let c = context(
        &p.hook("session-start", &ss("cs", "compact")),
        "SessionStart",
    );
    assert!(!c.contains("До сжатия"), "{c}");
}

/// Задача 9 (замечание ревью задачи 8): очереди изменённых файлов уходят до `GET /file-refs` —
/// индекс `file-refs.json` после `session-start` уже несёт устарелость по отправленным хэшам.
#[test]
fn session_start_flushes_before_file_refs() {
    let _g = serial();
    let (url, tok) = start("flush-order");
    let p = Proj::new("flush-order", &url, &tok);
    std::fs::create_dir_all(p.repo.join("src")).unwrap();
    std::fs::write(p.repo.join("src/pool.rs"), b"v1\n").unwrap();
    let o = p.zam(
        &[
            "log",
            "insight",
            "--kind",
            "gotcha",
            "--title",
            "Пул течёт",
            "--ref",
            "src/pool.rs",
        ],
        "Разобрано.\n".as_bytes(),
    );
    ok(&o);
    std::fs::write(p.repo.join("src/pool.rs"), b"v2\n").unwrap();
    let edit = post_ok(
        &p,
        "fo",
        "Edit",
        json!({"file_path": p.repo.join("src/pool.rs")}),
    );
    let _ = p.hook_at(DEAD, "post-tool", "claude", edit.to_string().as_bytes());
    assert_eq!(queue(&p, "fo").len(), 1);
    let ss = p.ev("SessionStart", "fo", json!({"source": "startup"}));
    let _ = p.hook_at(&url, "session-start", "claude", ss.to_string().as_bytes());
    assert!(queue(&p, "fo").is_empty(), "{}", p.log());
    let cache: Value =
        serde_json::from_str(&std::fs::read_to_string(p.cache().join("file-refs.json")).unwrap())
            .unwrap();
    assert_eq!(cache["refs"][0]["items"][0]["state"], "stale", "{cache}");
}

/// §9.3: пара «ошибка → исправление» и правки — единственное напоминание `Stop` содержит
/// «запиши gotcha/failure с error_regex: …», второго нет.
#[test]
fn checkpoint_reminds_fix_once() {
    let _g = serial();
    let (url, tok) = start("checkpoint-fix");
    let p = Proj::new("checkpoint-fix", &url, &tok);
    let _ = p.hook(
        "session-start",
        &p.ev("SessionStart", "fx", json!({"source": "startup"})),
    );
    fix_pair(&p, "fx");
    let stop = p.ev("Stop", "fx", json!({"stop_hook_active": false}));
    let v = hook_json(&p.hook("checkpoint", &stop));
    assert_eq!(v["decision"], "block", "{v}");
    let r = v["reason"].as_str().unwrap();
    assert!(r.contains("zam log changelog"), "{r}");
    let line = r
        .lines()
        .find(|l| l.starts_with("ZeroAIMemory: запиши gotcha/failure с error_regex: "))
        .unwrap_or_else(|| panic!("нет строки пары: {r}"));
    assert!(
        line.contains("cannot find value") && line.contains("(правки: src/a.rs)"),
        "{line}"
    );
    empty(&p.hook("checkpoint", &stop));
}

/// §9.3: после пары записан insight — CLI агента (`zam log insight`) или MCP `memory_log`
/// (`post-tool`) — строки пары в напоминании нет.
#[test]
fn checkpoint_no_fix_reminder_after_insight_write() {
    let _g = serial();
    let (url, tok) = start("checkpoint-nofix");
    let p = Proj::new("checkpoint-nofix", &url, &tok);
    let stop = |s: &str| {
        p.hook(
            "checkpoint",
            &p.ev("Stop", s, json!({"stop_hook_active": false})),
        )
    };
    // CLI в сессии хуков (session.json): запись insight — в журнал и `memory_writes`
    let _ = p.hook(
        "session-start",
        &p.ev("SessionStart", "nf-1", json!({"source": "startup"})),
    );
    fix_pair(&p, "nf-1");
    let o = p
        .cmd(&p.url)
        .env("CLAUDECODE", "1")
        .args([
            "log",
            "insight",
            "--kind",
            "gotcha",
            "--title",
            "E0425 после переименования",
        ])
        .write_stdin("Разбор.\n")
        .output()
        .unwrap();
    ok(&o);
    assert!(
        events(&p, "nf-1")
            .iter()
            .any(|e| e["ev"] == "memory_write" && e["kind"] == "insight")
    );
    empty(&stop("nf-1"));
    // MCP memory_log (успешный PostToolUse) — `memory_write` в журнал; сервер счётчик MCP здесь
    // не видит, поэтому общее напоминание остаётся, а строки пары нет
    let _ = p.hook(
        "session-start",
        &p.ev("SessionStart", "nf-2", json!({"source": "startup"})),
    );
    fix_pair(&p, "nf-2");
    let failed = p.ev(
        "PostToolUse",
        "nf-2",
        json!({"tool_name": "mcp__zeroaimemory__memory_log",
               "tool_input": {"kind": "insight", "title": "x"},
               "tool_response": {"isError": true}}),
    );
    let _ = p.hook_at(DEAD, "post-tool", "claude", failed.to_string().as_bytes());
    assert!(!events(&p, "nf-2").iter().any(|e| e["ev"] == "memory_write"));
    let mcp = post_ok(
        &p,
        "nf-2",
        "mcp__zeroaimemory__memory_log",
        json!({"kind": "insight", "title": "E0425"}),
    );
    empty(
        &p.hook_at(DEAD, "post-tool", "claude", mcp.to_string().as_bytes())
            .0,
    );
    assert!(
        events(&p, "nf-2")
            .iter()
            .any(|e| e["ev"] == "memory_write" && e["kind"] == "insight")
    );
    let v = hook_json(&stop("nf-2"));
    let r = v["reason"].as_str().unwrap_or_default();
    assert!(!r.contains("error_regex"), "{v}");
}

/// §9.3: тот же отпечаток ошибки третий раз за сессию — строка о повторе, раз на отпечаток;
/// другая ошибка считается отдельно; `post_tool` выключен — строка всё равно есть.
#[test]
fn post_tool_loop_third_time_hint() {
    let _g = serial();
    let p = Proj::new("loop", DEAD, "zam_x");
    let run = |s: &str, err: &str| {
        let ev = bash_fail(&p, s, "npm test", err);
        let (o, _) = p.hook_at(DEAD, "post-tool", "claude", ev.to_string().as_bytes());
        ok(&o)
    };
    const LOOP: &str = "ZeroAIMemory: эта ошибка повторяется";
    // номер строки в тексте ошибки не меняет отпечаток
    assert!(!run("lp", "Error: boom at a.js:10:5").contains(LOOP));
    assert!(!run("lp", "Error: boom at a.js:11:2").contains(LOOP));
    let third = run("lp", "Error: boom at a.js:12:1");
    let v: Value = serde_json::from_str(third.trim()).unwrap();
    let c = v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(
        c.contains(
            "ZeroAIMemory: эта ошибка повторяется 3-й раз в сессии — проверь память \
             (memory_search) или запиши разбор"
        ),
        "{c}"
    );
    assert!(
        !run("lp", "Error: boom at a.js:13:1").contains(LOOP),
        "раз на отпечаток"
    );
    for _ in 0..2 {
        assert!(!run("lp", "TypeError: other").contains(LOOP));
    }
    std::fs::write(
        p.repo.join(".zeroaimemory/config.toml"),
        "[hooks]\npost_tool = false\n",
    )
    .unwrap();
    assert!(run("lp", "TypeError: other").contains("повторяется 3-й раз"));
    // другая сессия — счёт заново
    assert!(!run("lp2", "Error: boom at a.js:1:1").contains(LOOP));
}

// ---------- M4.5, задача 10: очередь запусков субагентов, `subagent-start` ----------

/// `PreToolUse` запуска субагента (Claude: `Agent`/`Task`).
fn spawn_ev(p: &Proj, session: &str, tool: &str, agent_type: Option<&str>, prompt: &str) -> Value {
    let mut ti = json!({"description": "d", "prompt": prompt});
    if let Some(t) = agent_type {
        ti["subagent_type"] = json!(t);
    }
    p.ev(
        "PreToolUse",
        session,
        json!({"tool_name": tool, "tool_use_id": "u1", "tool_input": ti}),
    )
}

fn sub_ev(p: &Proj, session: &str, agent_id: &str, agent_type: &str) -> Value {
    p.ev(
        "SubagentStart",
        session,
        json!({"agent_id": agent_id, "agent_type": agent_type}),
    )
}

/// Очередь запусков сессии (`spawn-<сессия>.json`); нет файла — пусто.
fn spawns(p: &Proj, session: &str) -> Vec<Value> {
    let Some(f) = p.session_file("spawn", session, ".json") else {
        return Vec::new();
    };
    serde_json::from_str::<Vec<Value>>(&std::fs::read_to_string(f).unwrap()).unwrap()
}

/// `pre-tool` без сети на сервере `DEAD`.
fn pre_dead(p: &Proj, ev: &Value) -> Output {
    p.hook_at(DEAD, "pre-tool", "claude", ev.to_string().as_bytes())
        .0
}

/// `pre-tool` на `Agent`/`Task` пишет запуск в очередь: тип (нет — `general-purpose`) и первые
/// 2000 символов задачи; вывода нет. Выключенный `pre_tool` очередь не гасит, выключенный
/// `subagent_start` — гасит.
#[test]
fn pre_tool_agent_queues_spawn() {
    let _g = serial();
    let p = Proj::new("spawn-queue", DEAD, "zam_x");
    let long = format!("Найди кэш {}", "я".repeat(3000));
    empty(&pre_dead(
        &p,
        &spawn_ev(&p, "sq", "Agent", Some("Explore"), &long),
    ));
    empty(&pre_dead(&p, &spawn_ev(&p, "sq", "Task", None, "вторая")));
    let q = spawns(&p, "sq");
    assert_eq!(q.len(), 2, "{q:?}");
    assert_eq!(q[0]["agent_type"], "Explore");
    let task = q[0]["task"].as_str().unwrap();
    assert_eq!(task.chars().count(), 2000);
    assert!(task.starts_with("Найди кэш я"), "{task}");
    assert!(q[0]["at"].as_u64().unwrap() > 0);
    assert_eq!(q[1]["agent_type"], "general-purpose");
    assert_eq!(q[1]["task"], "вторая");
    // другой инструмент — не запуск
    empty(&pre_dead(&p, &spawn_ev(&p, "sq", "Bash", Some("x"), "y")));
    assert_eq!(spawns(&p, "sq").len(), 2);
    let cfg = p.repo.join(".zeroaimemory/config.toml");
    std::fs::write(&cfg, "[hooks]\npre_tool = false\n").unwrap();
    pre_dead(&p, &spawn_ev(&p, "sq", "Agent", Some("Plan"), "третья"));
    assert_eq!(spawns(&p, "sq").len(), 3);
    std::fs::write(&cfg, "[hooks]\nsubagent_start = false\n").unwrap();
    pre_dead(&p, &spawn_ev(&p, "sq", "Agent", Some("Plan"), "четвёртая"));
    assert_eq!(spawns(&p, "sq").len(), 3);
}

/// До 8 записей (старейшие отбрасываются); запись старше 10 минут не выдаётся и удаляется;
/// выданная запись уходит из очереди (тип — без учёта регистра).
#[test]
fn spawn_queue_ttl_and_cap() {
    let _g = serial();
    let p = Proj::new("spawn-ttl", DEAD, "zam_x");
    for i in 0..10 {
        pre_dead(
            &p,
            &spawn_ev(&p, "st", "Agent", Some("Explore"), &format!("t{i}")),
        );
    }
    let q = spawns(&p, "st");
    let tasks: Vec<&str> = q.iter().map(|s| s["task"].as_str().unwrap()).collect();
    assert_eq!(tasks, ["t2", "t3", "t4", "t5", "t6", "t7", "t8", "t9"]);
    // просроченная запись: время первой — 11 минут назад
    let f = p.session_file("spawn", "st", ".json").unwrap();
    let mut q = q.clone();
    let now = q[7]["at"].as_u64().unwrap();
    q[0]["at"] = json!(now - 11 * 60 * 1000);
    q[0]["agent_type"] = json!("Plan");
    std::fs::write(&f, serde_json::to_string(&q).unwrap()).unwrap();
    // сервер выключен: вывода нет, но очередь обработана
    let sub = |id: &str, t: &str| {
        p.hook_at(
            DEAD,
            "subagent-start",
            "claude",
            sub_ev(&p, "st", id, t).to_string().as_bytes(),
        )
        .0
    };
    empty(&sub("a1", "Plan"));
    let tasks: Vec<String> = spawns(&p, "st")
        .iter()
        .map(|s| s["task"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(tasks, ["t3", "t4", "t5", "t6", "t7", "t8", "t9"]);
    // старейшая подходящая уходит
    empty(&sub("a2", "explore"));
    let q = spawns(&p, "st");
    assert_eq!(q.len(), 6);
    assert_eq!(q[0]["task"], "t4");
}

/// Сервер с ловушками в Knowledge: запуск субагента `Plan` с задачей про пульсар — пакет под
/// задачу (только найденное), сессия `<session>:<agent_id>` с `parent_id` и `agent_type`,
/// событие `subagent` в журнале родителя; запись `Explore` остаётся в очереди.
#[test]
fn subagent_start_uses_matching_spawn() {
    let _g = serial();
    let (url, tok) = start("subagent-match");
    let p = Proj::new("subagent-match", &url, &tok);
    let pulsar = p.insight("Пульсар мигает по таймеру", "gotcha", "global", &[]);
    let quasar = p.insight("Квазар светит ярко", "gotcha", "global", &[]);
    for (t, task) in [("Explore", "квазар светит"), ("Plan", "пульсар мигает")]
    {
        p.hook("pre-tool", &spawn_ev(&p, "sm", "Agent", Some(t), task));
    }
    let c = context(
        &p.hook("subagent-start", &sub_ev(&p, "sm", "ag-1", "Plan")),
        "SubagentStart",
    );
    assert!(c.contains("### Ловушки"), "{c}");
    assert!(c.contains(&format!("i:{pulsar}")), "{c}");
    assert!(!c.contains(&format!("i:{quasar}")), "{c}");
    let q = spawns(&p, "sm");
    assert_eq!(q.len(), 1, "{q:?}");
    assert_eq!(q[0]["agent_type"], "Explore");
    let s = p.session("sm:ag-1");
    assert_eq!(s["parent_id"], "sm", "{s}");
    assert_eq!(s["agent_type"], "Plan", "{s}");
    assert_eq!(s["agent"], "claude-code", "{s}");
    assert_eq!(s["solution"], G, "{s}");
    let ev = events(&p, "sm");
    assert!(
        ev.iter()
            .any(|e| e["ev"] == "subagent" && e["agent_type"] == "Plan" && e["agent_id"] == "ag-1"),
        "{ev:?}"
    );
    // Codex: тот же формат вывода
    p.hook(
        "pre-tool",
        &spawn_ev(&p, "sm", "Agent", Some("Plan"), "пульсар мигает"),
    );
    let (o, _) = p.hook_at(
        &url,
        "subagent-start",
        "codex",
        sub_ev(&p, "sm", "ag-2", "Plan").to_string().as_bytes(),
    );
    assert!(context(&o, "SubagentStart").contains(&format!("i:{pulsar}")));
    // выключенный subagent_start — тишина
    std::fs::write(
        p.repo.join(".zeroaimemory/config.toml"),
        "[hooks]\nsubagent_start = false\n",
    )
    .unwrap();
    empty(&p.hook("subagent-start", &sub_ev(&p, "sm", "ag-3", "Explore")));
}

/// Нет подходящей записи в очереди — общий мини-дайджест (ловушки, без разделов пакета под
/// задачу); очередь другого типа не трогается; без `agent_id` — пакет в сессии родителя.
#[test]
fn subagent_start_without_spawn_generic() {
    let _g = serial();
    let (url, tok) = start("subagent-generic");
    let p = Proj::new("subagent-generic", &url, &tok);
    let pulsar = p.insight("Пульсар мигает по таймеру", "gotcha", "global", &[]);
    let quasar = p.insight("Квазар светит ярко", "gotcha", "global", &[]);
    p.hook(
        "pre-tool",
        &spawn_ev(&p, "sg", "Agent", Some("Explore"), "квазар"),
    );
    let c = context(
        &p.hook("subagent-start", &sub_ev(&p, "sg", "ag-1", "Plan")),
        "SubagentStart",
    );
    assert!(c.contains("### Ловушки"), "{c}");
    assert!(c.contains(&format!("i:{pulsar}")), "{c}");
    assert!(c.contains(&format!("i:{quasar}")), "{c}");
    for s in ["### Решения", "### Термины", "### Файлы"] {
        assert!(!c.contains(s), "{s}: {c}");
    }
    assert_eq!(spawns(&p, "sg").len(), 1);
    let ev = p.ev("SubagentStart", "sg2", json!({"agent_type": "Plan"}));
    let c = context(&p.hook("subagent-start", &ev), "SubagentStart");
    assert!(c.contains("### Ловушки"), "{c}");
    assert_eq!(p.session("sg2")["id"], "sg2");
}

/// Сервер недоступен — тишина и код 0; событие `subagent` в журнале есть, запись очереди взята.
#[test]
fn subagent_start_server_down_silent() {
    let _g = serial();
    let p = Proj::new("subagent-down", DEAD, "zam_x");
    pre_dead(&p, &spawn_ev(&p, "sd", "Agent", Some("Plan"), "пульсар"));
    assert_eq!(spawns(&p, "sd").len(), 1);
    let (o, d) = p.hook_at(
        DEAD,
        "subagent-start",
        "claude",
        sub_ev(&p, "sd", "a1", "Plan").to_string().as_bytes(),
    );
    empty(&o);
    assert!(d <= OTHER_MAX, "{d:?}");
    assert!(spawns(&p, "sd").is_empty());
    let ev = events(&p, "sd");
    assert!(ev.iter().any(|e| e["ev"] == "subagent"), "{ev:?}");
}

/// Бюджет 300 мс: с живым сервером p95 ≤ 1200 мс (×4, как `session-start`); сервер, который
/// принимает соединение и молчит, — тишина не дольше 1 с.
#[test]
fn subagent_start_budget_300ms() {
    let _g = serial();
    let (url, tok) = start("subagent-budget");
    let p = Proj::new("subagent-budget", &url, &tok);
    p.insight("Пульсар мигает по таймеру", "gotcha", "global", &[]);
    let mut times = Vec::new();
    let mut answered = 0;
    for i in 0..20 {
        let s = format!("sb{i}");
        p.hook(
            "pre-tool",
            &spawn_ev(&p, &s, "Agent", Some("Plan"), "пульсар мигает"),
        );
        let (o, d) = p.hook_at(
            &url,
            "subagent-start",
            "claude",
            sub_ev(&p, &s, "a1", "Plan").to_string().as_bytes(),
        );
        // отладочная сборка на HDD: запись учёта показа (fsync) изредка не укладывается в
        // бюджет — тогда хук молчит, что и проверяется; большинство ответов — с пакетом
        if !ok(&o).trim().is_empty() {
            answered += 1;
        }
        times.push(d);
    }
    assert!(answered >= 15, "с пакетом только {answered} из 20");
    let q = p95(times);
    eprintln!("ZAM_TIMING subagent-start p95={q:?}");
    assert!(q <= max_for("session-start"), "p95 {q:?}");
    // «чёрная дыра»: соединение принято, ответа нет
    let hole = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = format!("http://{}", hole.local_addr().unwrap());
    std::thread::spawn(move || {
        let mut keep = Vec::new();
        for s in hole.incoming().flatten() {
            keep.push(s);
        }
    });
    let (o, d) = p.hook_at(
        &addr,
        "subagent-start",
        "claude",
        sub_ev(&p, "sb-hole", "a1", "Plan").to_string().as_bytes(),
    );
    empty(&o);
    assert!(d <= OTHER_MAX, "{d:?}");
}

/// Ревью задачи 10 (M-2): `session-start` снимает просроченные записи очереди своей сессии, а
/// файл очереди без изменений дольше часа удаляется уборкой кэша (не через 7 дней).
#[test]
fn spawn_queue_pruned_on_session_start_and_gc() {
    let _g = serial();
    let p = Proj::new("spawn-gc", DEAD, "zam_x");
    for s in ["sp", "old"] {
        pre_dead(
            &p,
            &spawn_ev(&p, s, "Agent", Some("Plan"), &format!("{s}-1")),
        );
        pre_dead(
            &p,
            &spawn_ev(&p, s, "Agent", Some("Plan"), &format!("{s}-2")),
        );
    }
    // первая запись `sp` — 11 минут назад
    let f = p.session_file("spawn", "sp", ".json").unwrap();
    let mut q = spawns(&p, "sp");
    let now = q[1]["at"].as_u64().unwrap();
    q[0]["at"] = json!(now - 11 * 60 * 1000);
    std::fs::write(&f, serde_json::to_string(&q).unwrap()).unwrap();
    // файл `old` не менялся два часа
    let old = p.session_file("spawn", "old", ".json").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - Duration::from_secs(2 * 3600))
        .unwrap();
    let ev = p.ev("SessionStart", "sp", json!({"source": "startup"}));
    let (o, _) = p.hook_at(DEAD, "session-start", "claude", ev.to_string().as_bytes());
    ok(&o);
    let tasks: Vec<String> = spawns(&p, "sp")
        .iter()
        .map(|s| s["task"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(tasks, ["sp-2"]);
    assert!(!old.exists(), "файл очереди старше часа не удалён");
}

/// Очередь учёта токенов сессии (`tokens-<сессия>.jsonl`), строки JSON.
fn token_queue(p: &Proj, session: &str) -> Vec<Value> {
    let Some(f) = p.session_file("tokens", session, ".jsonl") else {
        return Vec::new();
    };
    std::fs::read_to_string(f)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// Оценка токенов клиента (как `zam_markdown::ApproxCounter`): ⌈символы / 3,5⌉.
fn approx_tokens(s: &str) -> u64 {
    (s.chars().count() as u64 * 2).div_ceil(7)
}

/// Хуки с локальным выводом (M4.5 §9.5): `pre-tool` копит выданное в очереди учёта без сети;
/// сервер недоступен — очередь цела; следующий сетевой хук (`prompt`) отправляет её пачкой
/// `POST /sessions {tokens}` и удаляет; `GET /sessions/{id}` отдаёт `pre_tool: {calls, tokens}`.
#[test]
fn local_tokens_flushed_by_hook() {
    let _g = serial();
    let (url, tok) = start("tokens");
    let p = Proj::new("tokens", &url, &tok);
    write_cache(&p, &url);
    let mut want = 0;
    let (o, _) = p.hook_at(
        &url,
        "pre-tool",
        "claude",
        write_ev(&p, "tk", "scripts/deploy.sh")
            .to_string()
            .as_bytes(),
    );
    want += approx_tokens(&context(&o, "PreToolUse"));
    let push = p.ev(
        "PreToolUse",
        "tk",
        json!({"tool_name": "Bash", "tool_input": {"command": "git push --force"}}),
    );
    let (o, _) = p.hook_at(&url, "pre-tool", "claude", push.to_string().as_bytes());
    want += approx_tokens(&context(&o, "PreToolUse"));
    // тишина — не выдача
    let quiet = p.ev(
        "PreToolUse",
        "tk",
        json!({"tool_name": "Bash", "tool_input": {"command": "ls"}}),
    );
    let (o, _) = p.hook_at(&url, "pre-tool", "claude", quiet.to_string().as_bytes());
    empty(&o);
    let q = token_queue(&p, "tk");
    assert_eq!(q.len(), 2, "{q:?}");
    assert!(q.iter().all(|l| l["e"] == "pre_tool"), "{q:?}");
    assert_eq!(
        q.iter().map(|l| l["t"].as_u64().unwrap()).sum::<u64>(),
        want
    );
    // сервер недоступен — очередь остаётся до следующего сетевого хука
    let pr = p.ev("UserPromptSubmit", "tk", json!({"prompt": "zzqx"}));
    let _ = p.hook_at(DEAD, "prompt", "claude", pr.to_string().as_bytes());
    assert_eq!(token_queue(&p, "tk").len(), 2, "ошибка — очередь цела");
    let _ = p.hook_at(&url, "prompt", "claude", pr.to_string().as_bytes());
    assert!(token_queue(&p, "tk").is_empty(), "очередь отправлена");
    let s = p.session("tk");
    assert_eq!(
        s["tokens"]["pre_tool"],
        json!({"calls": 2, "tokens": want}),
        "{s}"
    );
}
