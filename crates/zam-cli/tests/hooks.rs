//! Хуки `zam hook …` (§10.3; задача 10 M4) против настоящего zam-server в том же процессе тестов:
//! вывод по контракту Claude Code и Codex, локальный кэш триггеров (ETag), один показ за сессию,
//! напоминание `checkpoint` (R1), переключатели (R5), fail-open и время (R3: `pre-tool` ≤ 200 мс
//! p95, остальные ≤ 1 с). Сервер без моделей (поиск — FTS). Каталоги проекта и «домашний» — во
//! временном каталоге целей сборки (`CARGO_TARGET_TMPDIR`), не в профиле пользователя.
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
        let mut c = Command::cargo_bin("zam").unwrap();
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
        // окружение агента и клиентские переменные `zam` (`ZAM_SESSION`, `ZAM_AGENT`,
        // `ZAM_MACHINE`), в котором идут сами тесты, не должны утекать в `zam` — общий список
        for k in zam_core::AGENT_ENV_VARS
            .iter()
            .chain(&zam_core::CLIENT_ENV_VARS)
        {
            c.env_remove(k);
        }
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
