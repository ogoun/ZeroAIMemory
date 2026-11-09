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
    start_opt(name, false)
}

/// [`start`]; `rules_write` — агентам разрешена запись в Rules (`rules.agent_write`).
fn start_opt(name: &str, rules_write: bool) -> (String, String) {
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
            if rules_write {
                st.system
                    .set_setting("rules.agent_write", &true)
                    .await
                    .unwrap();
            }
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
        std::fs::create_dir_all(home.join(".zeroaimemory")).unwrap();
        std::fs::write(
            repo.join(".zeroaimemory/solution.toml"),
            format!("agent_solution_id = \"{G}\"\nname = \"Хуки\"\n"),
        )
        .unwrap();
        // ревью задачи 6, M8: хуки тестов не запускают настоящую `zam code sync --background`
        // (каталог теста занят, лишние запросы к тестовому серверу); автозапуск проверяют
        // тесты, снявшие это ([`Proj::auto_on`])
        std::fs::write(
            home.join(".zeroaimemory/config.toml"),
            "[code]\nauto = false\n",
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

    /// Автозапуск фоновой синхронизации карты — по умолчанию клиента (снимает `auto = false`).
    fn auto_on(&self) {
        let _ = std::fs::remove_file(self.home.join(".zeroaimemory/config.toml"));
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
        "ZeroAIMemory: для **/*.sh: Скрипты — только LF [gotcha·th] i:E1"
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
    assert!(context(&o, "PreToolUse").contains("Не форсить main [gotcha·th] i:E2"));
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
        c.contains(&format!("Скрипты — только LF [gotcha·hyp] i:{id}")),
        "{c}"
    );
    // R2: блок v1 устарел — сервер напоминает
    assert!(c.contains("устарел (v1 → v6)"), "{c}");
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

/// M6, задача 5 (A): привязка `zam mcp` к сессии (цепочка предков и файл `agent-<pid>.json`)
/// и прочая локальная уборка хука не тратят бюджет `session-start` до запроса: после `compact`
/// пакет восстановления приходит с сервера, даже когда привязка пишется медленно (подмена
/// `ZAM_TEST_BINDING_STALL_MS`, фича `test-hooks`), в том числе в проекте без файла привязки.
/// Раньше уборка шла до запроса, и задержка ≥ 270 мс давала «бюджет исчерпан» и пакет без
/// сервера (пилот ТС-2, `sh-01` r1: 1543 → 80 токенов).
#[test]
fn session_start_compact_budget_kept_with_slow_binding() {
    let _g = serial();
    let (url, tok) = start("ss-compact-budget");
    let p = Proj::new("ss-compact-budget", &url, &tok);
    let id = p.insight("Миграции — под замком", "gotcha", "solution", &[]);
    assert!(
        std::fs::read_dir(p.cache())
            .map(|rd| rd
                .flatten()
                .all(|e| !e.file_name().to_string_lossy().starts_with("agent-")))
            .unwrap_or(true),
        "в проекте ещё нет файла привязки"
    );
    let ev = p.ev("SessionStart", "ss-compact", json!({"source": "compact"}));
    let mut c = p.cmd(&url);
    c.env("ZAM_TEST_BINDING_STALL_MS", "600")
        .args(["hook", "session-start", "--agent", "claude"])
        .write_stdin(ev.to_string().into_bytes());
    let t = Instant::now();
    let o = c.output().unwrap();
    let took = t.elapsed();
    let log = p.log();
    assert!(!log.contains(zam_core::HOOK_BUDGET_SPENT), "{log}");
    let text = context(&o, "SessionStart");
    assert!(
        text.contains(&format!("i:{id}")),
        "пакет восстановления сервера: {text}\n{log}"
    );
    // хук ждёт запись привязки (ревью задачи 5, I-1), но не дольше потолка 1,5 с
    assert!(
        took < max_for("session-start") + Duration::from_millis(1500),
        "{took:?}"
    );
    // после выхода хука привязка `zam mcp` и `session.json` — уже новой сессии (§7.1)
    assert_eq!(binding_session(&p).as_deref(), Some("ss-compact"));
    let cur = std::fs::read_to_string(p.cache().join("session.json")).unwrap();
    assert!(cur.contains("ss-compact"), "{cur}");
    // `/clear` — новая сессия того же агента: привязка перезаписана, хотя запись медленная
    let ev = p.ev("SessionStart", "ss-after-clear", json!({"source": "clear"}));
    let mut c = p.cmd(&url);
    c.env("ZAM_TEST_BINDING_STALL_MS", "600")
        .args(["hook", "session-start", "--agent", "claude"])
        .write_stdin(ev.to_string().into_bytes());
    c.output().unwrap();
    assert_eq!(binding_session(&p).as_deref(), Some("ss-after-clear"));
    let cur = std::fs::read_to_string(p.cache().join("session.json")).unwrap();
    assert!(cur.contains("ss-after-clear"), "{cur}");
}

/// Сессия из файла привязки `zam mcp` (`agent-<pid>.json`): у `zam` в тесте агента среди
/// предков нет, файл пишется на непосредственного родителя — процесс теста.
fn binding_session(p: &Proj) -> Option<String> {
    let f = p.cache().join(format!("agent-{}.json", std::process::id()));
    let v: Value = serde_json::from_str(&std::fs::read_to_string(f).ok()?).ok()?;
    v["session_id"].as_str().map(str::to_string)
}

/// Ревью задачи 5, I-1: уборка большого кэша (`gc_cache` тысяч просроченных файлов) идёт после
/// записи привязки и не задерживает её: после выхода хука привязка — новой сессии, хук не
/// ждёт уборку дольше бюджета.
#[test]
fn session_start_big_cache_gc_does_not_delay_binding() {
    let _g = serial();
    let (url, tok) = start("ss-big-gc");
    let p = Proj::new("ss-big-gc", &url, &tok);
    std::fs::create_dir_all(p.cache()).unwrap();
    let old = std::time::SystemTime::now() - Duration::from_secs(30 * 24 * 3600);
    for i in 0..3000 {
        let f = p
            .cache()
            .join(format!("shown-old-{i}-0000000{}.json", i % 10));
        std::fs::write(&f, "{}").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&f)
            .unwrap()
            .set_modified(old)
            .unwrap();
    }
    // прогрев: первый запуск нового бинарника на Windows проверяет антивирус
    p.zam(&["--version"], b"");
    let ev = p.ev("SessionStart", "ss-big", json!({"source": "startup"}));
    let (o, took) = p.hook_at(&url, "session-start", "claude", ev.to_string().as_bytes());
    assert!(o.status.success());
    assert_eq!(binding_session(&p).as_deref(), Some("ss-big"));
    assert!(took < max_for("session-start"), "{took:?}");
    let log = p.log();
    assert!(!log.contains(zam_core::HOOK_BUDGET_SPENT), "{log}");
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
    let line =
        format!("ZeroAIMemory: эта ошибка уже разбиралась: CRLF в shebang [failure·hyp] i:{id}");
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
        c.contains("эта ошибка уже разбиралась: CRLF в shebang [gotcha·th] i:E3"),
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
    // M5.6: `code_map` выключен в клиенте — фоновой синхронизации карты нет
    git_head(&p, &sha(1));
    std::fs::write(
        p.repo.join(".zeroaimemory/config.toml"),
        "[hooks]
pre_tool = true
code_map = false
",
    )
    .unwrap();
    let ev = p.ev("SessionStart", "d", json!({"source": "startup"}));
    let _ = p.hook_at(DEAD, "session-start", "claude", ev.to_string().as_bytes());
    assert!(spawns_logged(&p).is_empty(), "{}", p.log());
    std::fs::write(
        p.repo.join(".zeroaimemory/config.toml"),
        "[hooks]
pre_tool = true
",
    )
    .unwrap();
    let ev = p.ev("SessionStart", "e", json!({"source": "startup"}));
    let _ = p.hook_at(DEAD, "session-start", "claude", ev.to_string().as_bytes());
    assert_eq!(spawns_logged(&p).len(), 1, "{}", p.log());
}

/// R3: время `session-start` (≤ 1200 мс), `prompt` и `post-tool` (≤ 1000 мс), `checkpoint`
/// (≤ 400 мс) с сервером — p95 из 20 запусков каждого; пустых ответов (хук замолчал по
/// внутреннему сроку, fail-open) — не больше 5 % всех 80 вызовов, как у
/// `m4_acceptance::hook_budgets`: на общем раннере CI отладочная сборка изредка не укладывается
/// в бюджет вместе с запуском процесса (ubuntu: 872cfc4, e32fe1f, d020f16; на стенде A/B p95
/// ~15 мс без пустых — журнал M5, «Финальное ревью ветки»).
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
    let (mut report, mut silent, mut calls) = (Vec::new(), Vec::new(), 0usize);
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
            calls += 1;
            if ok(&o).trim().is_empty() {
                silent.push(format!("{k} #{i} {d:?}"));
            }
            times.push(d);
        }
        let q = p95(times);
        report.push(format!("{k} p95={q:?}"));
        assert!(q <= max_for(k), "{k}: p95 {q:?}");
    }
    eprintln!("ZAM_TIMING {}; пустые: {silent:?}", report.join(", "));
    assert!(
        silent.len() * 20 <= calls,
        "пустые ответы {silent:?} из {calls} (> 5 %)"
    );
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

/// Успешный `post-tool` без сети — 100 запусков (чтение со знанием о файле и журналом, правка
/// файла с символьной ссылкой в кэше — разбор ≤ 20 мс, M5.6): p95 ≤ 200 мс (×4 от бюджета
/// 50 мс, как `pre_tool_under_50ms`); значения — в журнал этапа.
#[test]
fn post_tool_success_under_50ms() {
    let _g = serial();
    let p = Proj::new("post-timing", DEAD, "zam_x");
    write_symbol_refs(
        &p,
        DEAD,
        &[(
            "src/pool.rs",
            &[
                ("i:E7", "gotcha", "Пул соединений течёт", Some("Pool::get")),
                ("n:N1", "note", "Архитектура пула", None),
            ],
        )],
    );
    std::fs::create_dir_all(p.repo.join("src")).unwrap();
    let mut src = String::from(
        "pub struct Pool;
impl Pool {
    pub fn get(&self) -> u32 { 0 }
}
",
    );
    let mut k = 0;
    while src.len() < 64 * 1024 {
        src.push_str(&format!(
            "pub fn f{k}(x: u32) -> u32 {{
    x + {k}
}}
"
        ));
        k += 1;
    }
    std::fs::write(p.repo.join("src/pool.rs"), &src).unwrap();
    let line = "ZeroAIMemory: о файле src/pool.rs в памяти: `Pool::get`: Пул соединений течёт \
                [gotcha] — i:E7; Архитектура пула [note] — n:N1";
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
            assert_eq!(context(&o, "PostToolUse"), line);
        } else {
            empty(&o);
        }
        times.push(d);
    }
    // правки с символьной ссылкой: в очереди — хэши символов (или строка без них по отсечке)
    let q = queue(&p, "pt1");
    assert_eq!(q.len(), 1, "{q:?}");
    eprintln!(
        "ZAM_TIMING post-tool: символов в очереди {}",
        q[0]["symbols"].as_array().map_or(0, Vec::len)
    );
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
    // ревью задачи 10: release-сборка обязана ответить пакетом на все 20 вызовов, послабление
    // 15 из 20 — только для отладочной сборки
    let need = if cfg!(debug_assertions) { 15 } else { 20 };
    assert!(
        answered >= need,
        "с пакетом только {answered} из 20 (нужно {need})"
    );
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

/// Ревью задачи 12: очередь учёта токенов — жёсткий предел 64 КиБ. Полная очередь не растёт,
/// выдача хука не меняется, причина — строка в `hooks.log`.
#[test]
fn token_queue_capped_at_64k() {
    let _g = serial();
    let p = Proj::new("tokens-cap", DEAD, "zam_x");
    write_cache(&p, DEAD);
    let ev = write_ev(&p, "tc", "scripts/deploy.sh").to_string();
    let (o, _) = p.hook_at(DEAD, "pre-tool", "claude", ev.as_bytes());
    let first = context(&o, "PreToolUse");
    assert!(!first.is_empty(), "{o:?}");
    assert_eq!(token_queue(&p, "tc").len(), 1);
    let f = p.session_file("tokens", "tc", ".jsonl").unwrap();
    // почти полная очередь: следующая строка уже не помещается в 64 КиБ
    let line = "{\"e\":\"pre_tool\",\"t\":1}\n";
    let have = std::fs::metadata(&f).unwrap().len() as usize;
    let pad = line.repeat((64 * 1024 - have) / line.len());
    let mut text = std::fs::read_to_string(&f).unwrap();
    text.push_str(&pad);
    std::fs::write(&f, &text).unwrap();
    let before = std::fs::metadata(&f).unwrap().len();
    assert!(before <= 64 * 1024 && before + 30 > 64 * 1024, "{before}");
    // другой триггер — выдача есть, но в очередь уже не попадает
    let push = p.ev(
        "PreToolUse",
        "tc",
        json!({"tool_name": "Bash", "tool_input": {"command": "git push --force"}}),
    );
    let (o, _) = p.hook_at(DEAD, "pre-tool", "claude", push.to_string().as_bytes());
    assert!(!context(&o, "PreToolUse").is_empty(), "{o:?}");
    assert_eq!(
        std::fs::metadata(&f).unwrap().len(),
        before,
        "сверх предела не дописано"
    );
    assert!(
        p.log().contains("очередь учёта токенов полна (64 КиБ)"),
        "{}",
        p.log()
    );
}

// ---------- M5.6, задача 6: символы в хуках и автосинхронизация карты кода ----------

/// Элемент индекса: `(id, вид, заголовок, символ)`.
type Item<'a> = (&'a str, &'a str, &'a str, Option<&'a str>);

/// Индекс `file-refs.json` с элементами по путям.
fn write_symbol_refs(p: &Proj, server: &str, paths: &[(&str, &[Item])]) {
    let refs: Vec<Value> = paths
        .iter()
        .map(|(path, items)| {
            let items: Vec<Value> = items
                .iter()
                .map(|(id, kind, title, symbol)| {
                    let mut o = json!({"id": id, "kind": kind, "title": title, "state": "ok"});
                    if let Some(s) = symbol {
                        o["symbol"] = json!(s);
                    }
                    o
                })
                .collect();
            json!({"path": path, "items": items})
        })
        .collect();
    let c = json!({"etag": "s1", "fetched_at": "2026-11-03T00:00:00Z", "server": server,
                   "solution": G, "refs": refs});
    std::fs::create_dir_all(p.cache()).unwrap();
    std::fs::write(p.cache().join("file-refs.json"), c.to_string()).unwrap();
}

const LIB_RS: &[u8] = b"pub fn foo() -> u32 {\n    1\n}\n\npub fn bar() -> u32 {\n    2\n}\n";

#[test]
fn post_tool_edit_queues_symbol_hashes_for_symbol_refs() {
    let _g = serial();
    let p = Proj::new("post-edit-sym", DEAD, "zam_x");
    write_symbol_refs(
        &p,
        DEAD,
        &[
            ("src/lib.rs", &[("n:N1", "note", "О foo", Some("foo"))]),
            ("src/plain.rs", &[("i:I1", "gotcha", "Файл", None)]),
            ("src/gone.rs", &[("n:N2", "note", "О baz", Some("baz"))]),
        ],
    );
    std::fs::create_dir_all(p.repo.join("src")).unwrap();
    std::fs::write(p.repo.join("src/lib.rs"), LIB_RS).unwrap();
    std::fs::write(p.repo.join("src/plain.rs"), LIB_RS).unwrap();
    for f in ["src/lib.rs", "src/plain.rs", "src/gone.rs"] {
        let v = post_ok(
            &p,
            "sy1",
            "Edit",
            json!({"file_path": p.repo.join(f), "old_string": "1", "new_string": "1"}),
        );
        empty(
            &p.hook_at(DEAD, "post-tool", "claude", v.to_string().as_bytes())
                .0,
        );
    }
    let q = queue(&p, "sy1");
    assert_eq!(q.len(), 3, "{q:?}");
    assert_eq!(q[0]["path"], "src/lib.rs");
    assert_eq!(q[0]["hash"], blake(LIB_RS));
    // все символы файла, а не только из ссылок (решение составителя плана 5)
    let syms = q[0]["symbols"]
        .as_array()
        .unwrap_or_else(|| panic!("{q:?}"));
    let quals: Vec<&str> = syms.iter().map(|s| s["qual"].as_str().unwrap()).collect();
    assert_eq!(quals, ["foo", "bar"], "{q:?}");
    for s in syms {
        let b = s["body_hash"].as_str().unwrap();
        assert_eq!(b.len(), 16, "{s}");
        assert!(b.bytes().all(|c| c.is_ascii_hexdigit()), "{s}");
    }
    // у пути без символьных ссылок — без symbols (как M4.5)
    assert_eq!(q[1], json!({"path": "src/plain.rs", "hash": blake(LIB_RS)}));
    // удалённый — нули без символов (сервер: symbol_removed)
    assert_eq!(q[2], json!({"path": "src/gone.rs", "hash": "0".repeat(64)}));
}

#[test]
fn post_tool_edit_big_file_no_symbols() {
    let _g = serial();
    let p = Proj::new("post-edit-big", DEAD, "zam_x");
    write_symbol_refs(
        &p,
        DEAD,
        &[("src/big.rs", &[("n:N1", "note", "О foo", Some("foo"))])],
    );
    std::fs::create_dir_all(p.repo.join("src")).unwrap();
    let mut big = LIB_RS.to_vec();
    while big.len() <= 512 * 1024 {
        big.extend_from_slice(b"// padding padding padding padding padding padding padding\n");
    }
    std::fs::write(p.repo.join("src/big.rs"), &big).unwrap();
    let v = post_ok(
        &p,
        "big1",
        "Write",
        json!({"file_path": p.repo.join("src/big.rs"), "content": "x"}),
    );
    empty(
        &p.hook_at(DEAD, "post-tool", "claude", v.to_string().as_bytes())
            .0,
    );
    assert_eq!(
        queue(&p, "big1"),
        vec![json!({"path": "src/big.rs", "hash": blake(&big)})]
    );
}

/// Обе формы строки знания о файле (решение составителя плана 13): только ссылки на один
/// символ — «о `Sym` в <путь>», смесь — «о файле <путь>: `Sym`: …» (символьные первыми).
#[test]
fn file_hint_names_symbol() {
    let _g = serial();
    let p = Proj::new("hint-sym", DEAD, "zam_x");
    write_symbol_refs(
        &p,
        DEAD,
        &[
            (
                "src/store.rs",
                &[
                    ("i:I1", "gotcha", "Открытие базы", Some("Store::open")),
                    ("n:N1", "note", "Схема", Some("Store::open")),
                ],
            ),
            (
                "src/api.rs",
                &[
                    ("c:C1", "changelog", "Маршруты", None),
                    ("i:I2", "gotcha", "Ошибки", Some("ApiError")),
                ],
            ),
        ],
    );
    let read = |s: &str, f: &str| {
        let v = post_ok(&p, s, "Read", json!({"file_path": p.repo.join(f)}));
        context(
            &p.hook_at(DEAD, "post-tool", "claude", v.to_string().as_bytes())
                .0,
            "PostToolUse",
        )
    };
    assert_eq!(
        read("h1", "src/store.rs"),
        "ZeroAIMemory: о `Store::open` в src/store.rs в памяти: Открытие базы [gotcha] — i:I1; \
         Схема [note] — n:N1"
    );
    assert_eq!(
        read("h1", "src/api.rs"),
        "ZeroAIMemory: о файле src/api.rs в памяти: `ApiError`: Ошибки [gotcha] — i:I2; \
         Маршруты [changelog] — c:C1"
    );
    // pre-tool перед правкой — та же строка
    let pre = p.ev(
        "PreToolUse",
        "h2",
        json!({"tool_name": "Edit", "tool_input": {"file_path": p.repo.join("src/store.rs")}}),
    );
    let c = context(
        &p.hook_at(DEAD, "pre-tool", "claude", pre.to_string().as_bytes())
            .0,
        "PreToolUse",
    );
    assert!(
        c.starts_with("ZeroAIMemory: о `Store::open` в src/store.rs в памяти:"),
        "{c}"
    );
}

/// §17 п. 12: кэш `file-refs.json` без поля `symbol` (клиент M4.5) читается.
#[test]
fn old_file_refs_cache_without_symbol_reads() {
    let _g = serial();
    let p = Proj::new("old-refs-cache", DEAD, "zam_x");
    write_file_refs(&p, DEAD);
    let raw = std::fs::read_to_string(p.cache().join("file-refs.json")).unwrap();
    assert!(!raw.contains("symbol"), "{raw}");
    let v = post_ok(
        &p,
        "old1",
        "Read",
        json!({"file_path": p.repo.join("src/pool.rs")}),
    );
    let (o, _) = p.hook_at(DEAD, "post-tool", "claude", v.to_string().as_bytes());
    assert_eq!(context(&o, "PostToolUse"), POOL_LINE);
}

/// Строки очереди `changed-*.jsonl` без `symbols` (клиент M4.5) и с ними — обе уходят на сервер.
#[test]
fn old_changed_queue_line_without_symbols_reads() {
    let _g = serial();
    let (url, tok) = start("old-queue");
    let p = Proj::new("old-queue", &url, &tok);
    std::fs::create_dir_all(p.repo.join("src")).unwrap();
    std::fs::write(p.repo.join("src/x.rs"), b"x").unwrap();
    let o = p.zam(
        &[
            "log", "insight", "--kind", "gotcha", "--title", "О x", "--scope", "solution", "--ref",
            "src/x.rs",
        ],
        "Подробно.\n".as_bytes(),
    );
    ok(&o);
    let line_old = json!({"path": "src/x.rs", "hash": "1".repeat(64)});
    let line_new = json!({"path": "src/y.rs", "hash": "2".repeat(64),
                          "symbols": [{"qual": "foo", "body_hash": "a".repeat(16)}]});
    // очереди создаёт хук правки (имя файла — с хэшем id); затем строки подменяются
    for (s, l) in [("oq1", &line_old), ("oq2", &line_new)] {
        let v = post_ok(
            &p,
            s,
            "Write",
            json!({"file_path": p.repo.join("src/x.rs")}),
        );
        let _ = p.hook_at(DEAD, "post-tool", "claude", v.to_string().as_bytes());
        let f = p.session_file("changed", s, ".jsonl").expect("очередь");
        std::fs::write(&f, format!("{l}\n")).unwrap();
    }
    let o = p.hook(
        "session-start",
        &p.ev("SessionStart", "oq3", json!({"source": "startup"})),
    );
    ok(&o);
    assert!(
        p.session_file("changed", "oq1", ".jsonl").is_none(),
        "{}",
        p.log()
    );
    assert!(
        p.session_file("changed", "oq2", ".jsonl").is_none(),
        "{}",
        p.log()
    );
    assert!(!p.log().contains("files/changed"), "{}", p.log());
    let idx = file_refs_index(&p);
    assert_eq!(ref_state(&idx, "src/x.rs"), ["stale"], "{idx}");
}

/// Ненастроенный адрес: фоновая `zam code sync` запускается (хуку сервер для запуска не нужен)
/// и сразу выходит с ошибкой конфигурации — без замка и сети.
const NO_URL: &str = "no-url";

/// Фиктивный `.git` проекта: `HEAD` → ветка `main` с коммитом `sha`.
fn git_head(p: &Proj, sha: &str) {
    let g = p.repo.join(".git");
    std::fs::create_dir_all(g.join("refs/heads")).unwrap();
    std::fs::write(g.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::write(g.join("refs/heads/main"), format!("{sha}\n")).unwrap();
}

/// Запуски фоновой синхронизации в `hooks.log`: время запуска, мс.
fn spawns_logged(p: &Proj) -> Vec<f64> {
    p.log()
        .lines()
        .filter_map(|l| {
            let rest = l.split("фоновая синхронизация карты запущена за ").nth(1)?;
            rest.split_whitespace().next()?.parse().ok()
        })
        .collect()
}

fn sha(n: u32) -> String {
    format!("{n:040x}")
}

#[test]
fn session_start_spawns_background_sync_once_per_interval() {
    let _g = serial();
    let p = Proj::new("auto-sync", NO_URL, "zam_x");
    p.auto_on();
    git_head(&p, &sha(1));
    let start = |s: &str| {
        let ev = p.ev("SessionStart", s, json!({"source": "startup"}));
        p.hook_at(NO_URL, "session-start", "claude", ev.to_string().as_bytes())
    };
    let _ = start("a1");
    assert_eq!(spawns_logged(&p).len(), 1, "{}", p.log());
    // в интервале и при том же HEAD — не стартует
    let _ = start("a2");
    assert_eq!(spawns_logged(&p).len(), 1, "{}", p.log());
    // смена HEAD (файл ветки) — стартует
    git_head(&p, &sha(2));
    let _ = start("a3");
    assert_eq!(spawns_logged(&p).len(), 2, "{}", p.log());
    // ветка только в packed-refs
    std::fs::remove_file(p.repo.join(".git/refs/heads/main")).unwrap();
    std::fs::write(
        p.repo.join(".git/packed-refs"),
        format!(
            "# pack-refs with: peeled fully-peeled sorted\n{} refs/heads/main\n",
            sha(3)
        ),
    )
    .unwrap();
    let _ = start("a4");
    assert_eq!(spawns_logged(&p).len(), 3, "{}", p.log());
    // замок занят (идёт синхронизация — блокировка ОС) — не стартует даже при новом HEAD
    let lock = p.cache().join("code-sync.lock");
    let held = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock)
        .unwrap();
    held.lock().unwrap();
    git_head(&p, &sha(4));
    let _ = start("a5");
    assert_eq!(spawns_logged(&p).len(), 3, "{}", p.log());
    // отпущен, файл остался — стартует
    held.unlock().unwrap();
    drop(held);
    git_head(&p, &sha(5));
    let _ = start("a6");
    assert_eq!(spawns_logged(&p).len(), 4, "{}", p.log());
    // 20 запусков, каждый со сменой HEAD (отсоединённый): время запуска p95 ≤ 10 мс
    let mut hook_times = Vec::new();
    for i in 0..20 {
        std::fs::write(p.repo.join(".git/HEAD"), format!("{}\n", sha(200 + i))).unwrap();
        let (o, d) = start(&format!("t{i}"));
        empty(&o);
        hook_times.push(d);
    }
    let t: Vec<f64> = spawns_logged(&p).into_iter().skip(4).collect();
    assert_eq!(t.len(), 20, "{}", p.log());
    let mut sorted = t.clone();
    sorted.sort_by(f64::total_cmp);
    let q = sorted[(sorted.len() * 95).div_ceil(100) - 1];
    eprintln!(
        "ZAM_TIMING запуск фоновой синхронизации p95={q:.2} мс; хук p95={:?}",
        p95(hook_times.clone())
    );
    // ≤ 10 мс (§10) — у release; отладочная сборка под нагрузкой соседних тестов — ×4, как
    // `PRE_TOOL_P95_MAX` (замер release — в журнале этапа)
    let max = if cfg!(debug_assertions) { 40.0 } else { 10.0 };
    assert!(q <= max, "запуск p95 {q} мс: {t:?}");
    // хук не ждёт фоновый процесс
    assert!(p95(hook_times) <= max_for("session-start"));
}

#[test]
fn checkpoint_spawns_only_after_edits() {
    let _g = serial();
    let p = Proj::new("auto-checkpoint", NO_URL, "zam_x");
    p.auto_on();
    git_head(&p, &sha(1));
    let stop = |s: &str| {
        let ev = p.ev("Stop", s, json!({"stop_hook_active": false}));
        p.hook_at(NO_URL, "checkpoint", "claude", ev.to_string().as_bytes())
    };
    let _ = stop("c1");
    assert!(spawns_logged(&p).is_empty(), "без правок: {}", p.log());
    p.hook_at(
        NO_URL,
        "pre-tool",
        "claude",
        write_ev(&p, "c1", "a.txt").to_string().as_bytes(),
    );
    let _ = stop("c1");
    assert_eq!(spawns_logged(&p).len(), 1, "после правок: {}", p.log());
    // повтор в интервале — нет
    let _ = stop("c1");
    assert_eq!(spawns_logged(&p).len(), 1, "{}", p.log());
}

#[test]
fn code_map_flag_off_no_spawn() {
    let _g = serial();
    let p = Proj::new("auto-off", NO_URL, "zam_x");
    p.auto_on();
    git_head(&p, &sha(1));
    let start = |s: &str| {
        let ev = p.ev("SessionStart", s, json!({"source": "startup"}));
        let _ = p.hook_at(NO_URL, "session-start", "claude", ev.to_string().as_bytes());
    };
    // тип подсказок `code_map` выключен на сервере (кэш hooks.json)
    std::fs::create_dir_all(p.cache()).unwrap();
    std::fs::write(
        p.cache().join("hooks.json"),
        json!({"fetched_at": "x", "server": NO_URL, "hooks": {"code_map": false}}).to_string(),
    )
    .unwrap();
    start("f1");
    assert!(spawns_logged(&p).is_empty(), "{}", p.log());
    std::fs::remove_file(p.cache().join("hooks.json")).unwrap();
    // `[hooks] code_map = false`, `code.auto = false`, `code.enabled = false`
    for (i, cfg) in [
        "[hooks]\ncode_map = false\n",
        "[code]\nauto = false\n",
        "[code]\nenabled = false\n",
    ]
    .iter()
    .enumerate()
    {
        std::fs::write(p.repo.join(".zeroaimemory/config.toml"), cfg).unwrap();
        git_head(&p, &sha(10 + i as u32));
        start("f2");
        assert!(spawns_logged(&p).is_empty(), "{cfg}: {}", p.log());
    }
    // без решения — нет
    std::fs::remove_file(p.repo.join(".zeroaimemory/config.toml")).unwrap();
    std::fs::remove_file(p.repo.join(".zeroaimemory/solution.toml")).unwrap();
    git_head(&p, &sha(77));
    start("f3");
    assert!(spawns_logged(&p).is_empty(), "{}", p.log());
}

/// Ревью задачи 6, I1: очередь из 30 файлов × 1500 символов (≈ 2,5 МБ) уходит пачками по
/// байтам без потерь — каждая символьная ссылка получает `symbol_changed`.
#[test]
fn changed_queue_with_many_symbols_flushed_without_loss() {
    let _g = serial();
    let (url, tok) = start("big-queue");
    let p = Proj::new("big-queue", &url, &tok);
    std::fs::create_dir_all(p.repo.join("src")).unwrap();
    let mut args: Vec<String> = ["log", "insight", "--kind", "gotcha", "--title", "Много"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    for i in 0..30 {
        std::fs::write(
            p.repo.join(format!("src/f{i}.rs")),
            b"pub fn f0() -> u32 {\n    0\n}\n",
        )
        .unwrap();
        args.push("--ref".into());
        args.push(format!("src/f{i}.rs#f0"));
    }
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    ok(&p.zam(&a, "Подробно.\n".as_bytes()));
    // очередь сессии: у каждого файла 1500 символов, тело `f0` другое
    let v = post_ok(
        &p,
        "bq1",
        "Write",
        json!({"file_path": p.repo.join("src/f0.rs")}),
    );
    let _ = p.hook_at(DEAD, "post-tool", "claude", v.to_string().as_bytes());
    let q = p.session_file("changed", "bq1", ".jsonl").expect("очередь");
    let mut text = String::new();
    for i in 0..30 {
        let mut syms: Vec<Value> = (0..1500)
            .map(|k| json!({"qual": format!("other_function_{k:05}"), "body_hash": format!("{k:016x}")}))
            .collect();
        syms.push(json!({"qual": "f0", "body_hash": "f".repeat(16)}));
        text.push_str(
            &json!({"path": format!("src/f{i}.rs"), "hash": "1".repeat(64), "symbols": syms})
                .to_string(),
        );
        text.push('\n');
    }
    assert!(text.len() > 2 * 1024 * 1024, "{}", text.len());
    std::fs::write(&q, text).unwrap();
    for i in 0..20 {
        if p.session_file("changed", "bq1", ".jsonl").is_none() {
            break;
        }
        let ev = p.ev(
            "SessionStart",
            &format!("bqs{i}"),
            json!({"source": "startup"}),
        );
        ok(&p.hook("session-start", &ev));
    }
    assert!(
        p.session_file("changed", "bq1", ".jsonl").is_none(),
        "очередь не ушла: {}",
        p.log()
    );
    assert!(!p.log().contains("отброшена"), "{}", p.log());
    let idx = file_refs_index(&p);
    for i in 0..30 {
        assert_eq!(
            ref_state(&idx, &format!("src/f{i}.rs")),
            ["stale"],
            "f{i}: {idx}"
        );
    }
}

/// Ревью задачи 6: частичный сбой отправки очереди пачками — постоянная ошибка на второй пачке
/// или тайм-аут посередине: первая пачка снята, остаток возвращён в очередь без потерь и
/// повторов.
#[test]
fn changed_flush_partial_failure_returns_rest() {
    let _g = serial();
    for (name, code, delay) in [
        ("flush-part-500", 500u16, Duration::ZERO),
        ("flush-part-slow", 200, Duration::from_secs(3)),
    ] {
        let (url, seen) =
            common::stub_with(std::sync::Arc::new(move |_: &str, path: &str, n: usize| {
                if path == "/api/v1/files/changed" && n >= 1 {
                    (code, r#"{"status":500,"detail":"x"}"#, delay)
                } else {
                    (200, r#"{"stale":0}"#, Duration::ZERO)
                }
            }));
        let p = Proj::new(name, &url, "zam_x");
        std::fs::create_dir_all(p.cache()).unwrap();
        let all: Vec<String> = (0..8).map(|i| format!("src/f{i}.rs")).collect();
        let mut text = String::new();
        for path in &all {
            let syms: Vec<Value> = (0..1500)
                .map(|k| json!({"qual": format!("other_function_{k:05}"), "body_hash": format!("{k:016x}")}))
                .collect();
            text.push_str(
                &json!({"path": path, "hash": "1".repeat(64), "symbols": syms}).to_string(),
            );
            text.push('\n');
        }
        let part = format!("pq-{}", &blake(b"pq")[..8]);
        std::fs::write(p.cache().join(format!("changed-{part}.jsonl")), text).unwrap();
        let ss = p.ev("SessionStart", "pq-new", json!({"source": "startup"}));
        let _ = p.hook_at(&url, "session-start", "claude", ss.to_string().as_bytes());
        let posts: Vec<Value> = seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, path, _)| m == "POST" && path == "/api/v1/files/changed")
            .map(|(_, _, b)| b.clone())
            .collect();
        assert!(posts.len() >= 2, "{name}: пачек {}", posts.len());
        let sent: Vec<String> = posts[0]["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["path"].as_str().unwrap().to_string())
            .collect();
        let left: Vec<String> = queue(&p, "pq")
            .iter()
            .map(|f| f["path"].as_str().unwrap().to_string())
            .collect();
        assert!(
            !sent.is_empty() && !left.is_empty(),
            "{name}: {sent:?} / {left:?}"
        );
        let mut both: Vec<String> = sent.iter().chain(&left).cloned().collect();
        both.sort();
        assert_eq!(both, all, "{name}: первая пачка снята, остаток возвращён");
    }
}

// ---- эпохи `session_seen` (M6 §7.2, задача 7a) ----

/// Пометка `/read` о повторной выдаче (§7.3).
const REREAD: &str = "· уже выдавалось в этой сессии";

/// Причины, по которым запрос новой эпохи не дошёл, по строкам `hooks.log`: промах бюджета до
/// запроса (`HOOK_BUDGET_SPENT` — запроса не было), сеть или срок запроса (`sessions(new_epoch):
/// сеть` — сервер мог успеть начать эпоху), ответ с ошибкой (`sessions(new_epoch): HTTP` —
/// эпохи нет).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct EpochMisses {
    budget: usize,
    net: usize,
    http: usize,
}

impl EpochMisses {
    fn of(log: &str) -> EpochMisses {
        EpochMisses {
            budget: log.matches(zam_core::HOOK_BUDGET_SPENT).count(),
            net: log.matches("sessions(new_epoch): сеть").count(),
            http: log.matches("sessions(new_epoch): HTTP").count(),
        }
    }

    fn since(self, before: EpochMisses) -> EpochMisses {
        EpochMisses {
            budget: self.budget - before.budget,
            net: self.net - before.net,
            http: self.http - before.http,
        }
    }
}

/// `PreCompact` (`checkpoint`, M6 §7.2: «в бюджете хука 100 мс»): `POST /sessions {id,
/// new_epoch: true}` идёт с тайм-аутом по остатку бюджета (100 мс − резерв 30 мс = 70 мс на
/// HTTP) — медленный сервер хук не задерживает; и без `SessionStart` (сессии ещё нет), и после
/// него; вывод — тишина. Время процесса отладочной сборки — по правилу файла (×4, 400 мс).
#[test]
fn checkpoint_precompact_new_epoch_in_hook_budget() {
    let _g = serial();
    let pc = |p: &Proj, s: &str| {
        p.ev(
            "PreCompact",
            s,
            json!({"trigger": "auto", "custom_instructions": ""}),
        )
        .to_string()
    };
    // медленный сервер (ответ через 2 с): хук не ждёт его дольше своего бюджета HTTP
    let (slow, seen) = common::stub(201, r#"{"id":"ps-1","epoch":1}"#, Duration::from_secs(2));
    let q = Proj::new("precompact-slow", &slow, "zam_x");
    let warm = q.ev("Stop", "ps-warm", json!({"stop_hook_active": false}));
    let _ = q.hook_at(&slow, "checkpoint", "claude", warm.to_string().as_bytes());
    let (o, d) = q.hook_at(&slow, "checkpoint", "claude", pc(&q, "ps-1").as_bytes());
    empty(&o);
    assert!(
        d <= max_for("checkpoint"),
        "медленный сервер задержал хук: {d:?}"
    );
    let m = EpochMisses::of(&q.log());
    let posts = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|(m, path, _)| m == "POST" && path == "/api/v1/sessions")
        .count();
    // запрос ушёл и снят по сроку — или до него не дошло (промах бюджета старта процесса)
    assert!(
        (posts == 1 && m.net == 1) || (posts == 0 && m.budget >= 1),
        "запросов {posts}, {m:?}: {}",
        q.log()
    );

    let (url, tok) = start("precompact-epoch");
    let p = Proj::new("precompact-epoch", &url, &tok);
    // прогрев процесса `zam` (холодный запуск с HDD — не бюджет хука)
    let warm = p.ev("Stop", "pe-warm", json!({"stop_hook_active": false}));
    empty(&p.hook("checkpoint", &warm));
    // `n` вызовов `PreCompact` сессии `s`: эпоха растёт на число дошедших — от `ok` до
    // `ok + net` (срок запроса мог истечь после того, как сервер её начал); промах бюджета
    // отладочной сборки на занятом HDD (старт процесса дольше 70 мс) — поведение продукта
    // не меньше `n` вызовов, и пока ни один не дошёл без промаха — ещё, всего до 5: эпоха
    // обязана начаться хотя бы одним подтверждённым запросом (повторное ревью 7a, I-B)
    let run = |s: &str, n: usize| -> (usize, EpochMisses) {
        let before = EpochMisses::of(&p.log());
        let mut calls = 0;
        loop {
            let (o, d) = p.hook_at(&url, "checkpoint", "claude", pc(&p, s).as_bytes());
            empty(&o);
            assert!(d <= max_for("checkpoint"), "checkpoint {d:?}");
            calls += 1;
            let m = EpochMisses::of(&p.log()).since(before);
            assert_eq!(m.http, 0, "ответ сервера с ошибкой: {}", p.log());
            let ok = calls - m.budget - m.net;
            if (calls >= n && ok >= 1) || calls >= 5 {
                assert!(
                    ok >= 1,
                    "ни одного дошедшего запроса из {calls}: {}",
                    p.log()
                );
                return (ok, m);
            }
        }
    };
    let in_range = |s: &str, from: usize, m: EpochMisses| {
        let e = p.session(s)["seen_epoch"].as_u64().unwrap() as usize;
        assert!(
            (from..=from + m.net).contains(&e),
            "{s}: эпоха {e}, дошло {from}, {m:?}: {}",
            p.log()
        );
        e
    };
    // без `SessionStart`: сессия создаётся (каждый дошедший вызов — +1 от 0)
    let (ok, m) = run("pe-0", 3);
    assert!(in_range("pe-0", ok, m) >= ok);
    // сессия есть (регистрация, как у `session-start`): каждый `PreCompact` — +1
    let r = http()
        .post(&format!("{url}/api/v1/sessions"))
        .header("Authorization", format!("Bearer {tok}"))
        .content_type("application/json")
        .send(r#"{"id":"pe-1","agent":"claude"}"#)
        .unwrap();
    assert_eq!(r.status(), 201);
    assert_eq!(p.session("pe-1")["seen_epoch"], 0);
    let (ok, m) = run("pe-1", 3);
    let e = in_range("pe-1", ok, m);
    assert!(e >= ok && ok >= 1, "эпоха {e}, дошло {ok}");
    // `Stop` эпоху не меняет
    let st = p.ev("Stop", "pe-1", json!({"stop_hook_active": false}));
    empty(&p.hook("checkpoint", &st));
    assert_eq!(p.session("pe-1")["seen_epoch"], e);
}

/// Старый сервер (v0.0.10-m5.7) отвечает на `new_epoch` 400 (`deny_unknown_fields`, «unknown
/// field `new_epoch`»): хук молчит, код 0, в `hooks.log` — ничего; запрос — отдельный, только
/// `{id, new_epoch}` (§13). Другой 400 — строка `sessions(new_epoch): HTTP 400` в журнале.
#[test]
fn old_server_new_epoch_400_silent() {
    let _g = serial();
    let (url, seen) = common::stub(
        400,
        r#"{"type":"/problems/bad-request","status":400,"detail":"некорректное тело: unknown field `new_epoch`, expected one of `id`, `agent`"}"#,
        Duration::ZERO,
    );
    let p = Proj::new("old-epoch", &url, "zam_x");
    let pc = p.ev(
        "PreCompact",
        "oe-1",
        json!({"trigger": "manual", "custom_instructions": ""}),
    );
    let (o, d) = p.hook_at(&url, "checkpoint", "claude", pc.to_string().as_bytes());
    empty(&o);
    assert!(d <= max_for("checkpoint"), "checkpoint {d:?}");
    let sent = seen.lock().unwrap().clone();
    let posts: Vec<&Value> = sent
        .iter()
        .filter(|(m, path, _)| m == "POST" && path == "/api/v1/sessions")
        .map(|(_, _, v)| v)
        .collect();
    let log = p.log();
    if log.contains(zam_core::HOOK_BUDGET_SPENT) {
        // промах бюджета старта процесса: запроса не было
        assert!(posts.is_empty(), "{sent:?}");
    } else {
        assert_eq!(
            posts,
            [&json!({"id": "oe-1", "new_epoch": true})],
            "{sent:?}"
        );
    }
    assert!(!log.contains("new_epoch"), "{log}");
    // 400 по другой причине — не «старый сервер»: в журнал с кодом
    let (url, _) = common::stub(
        400,
        r#"{"type":"/problems/bad-request","status":400,"detail":"id сессии «unattributed» зарезервирован"}"#,
        Duration::ZERO,
    );
    let p = Proj::new("other-400-epoch", &url, "zam_x");
    // событие — со своим `cwd`: журнал хука лежит в проекте события
    let pc = p.ev(
        "PreCompact",
        "oe-2",
        json!({"trigger": "manual", "custom_instructions": ""}),
    );
    let (o, _) = p.hook_at(&url, "checkpoint", "claude", pc.to_string().as_bytes());
    empty(&o);
    let log = p.log();
    assert!(
        log.contains("sessions(new_epoch): HTTP 400") || log.contains(zam_core::HOOK_BUDGET_SPENT),
        "{log}"
    );
}

/// M6 §7.2, ревью задачи 7a (I1): выключенный в клиенте `recovery` убирает `source` из тела
/// `/recall`, и сервер эпоху не начинает — тогда `session-start` с `source` `compact | resume |
/// clear` шлёт `POST /sessions {id, new_epoch: true}` сам; `startup` эпоху не меняет.
#[test]
fn session_start_recovery_off_new_epoch() {
    let _g = serial();
    let (url, tok) = start("ss-recovery-off");
    let p = Proj::new("ss-recovery-off", &url, &tok);
    std::fs::write(
        p.repo.join(".zeroaimemory/config.toml"),
        "[hooks]\nrecovery = false\n",
    )
    .unwrap();
    let ss = |s: &str, src: &str| {
        p.hook(
            "session-start",
            &p.ev("SessionStart", s, json!({"source": src})),
        )
    };
    // регистрация — прямым запросом: промах бюджета `startup` тест не касается
    let r = http()
        .post(&format!("{url}/api/v1/sessions"))
        .header("Authorization", format!("Bearer {tok}"))
        .content_type("application/json")
        .send(r#"{"id":"ro-1","agent":"claude"}"#)
        .unwrap();
    assert_eq!(r.status(), 201);
    assert_eq!(p.session("ro-1")["seen_epoch"], 0);
    let mut want = 0usize;
    for src in ["compact", "resume", "clear", "startup"] {
        let before = EpochMisses::of(&p.log());
        ss("ro-1", src);
        let m = EpochMisses::of(&p.log()).since(before);
        assert_eq!(m.http, 0, "{}", p.log());
        let e = p.session("ro-1")["seen_epoch"].as_u64().unwrap() as usize;
        if src == "startup" {
            assert_eq!(e, want, "startup эпоху не меняет");
        } else {
            // дошло — +1; промах бюджета — +0; срок запроса — +0 или +1
            let lo = want + usize::from(m.budget == 0 && m.net == 0);
            assert!(
                (lo..=want + 1).contains(&e),
                "{src}: эпоха {e}, было {want}, {m:?}: {}",
                p.log()
            );
        }
        want = e;
    }
    assert!(want >= 1, "ни одной эпохи: {}", p.log());
}

/// §17 п. 7 через хуки и `zam read --batch` (пачка — `POST /read`; побайтное чтение без пачки
/// идёт через `GET /nodes` и выданное не отмечает): чтение → повтор с пометкой → `PreCompact`
/// и `SessionStart compact` → чтение без пометки (новая эпоха), повтор — снова с пометкой.
#[test]
fn compact_resets_seen() {
    let _g = serial();
    let (url, tok) = start("compact-seen");
    let p = Proj::new("compact-seen", &url, &tok);
    let n = id_of(&ok(&p.zam(
        &["write", "knowledge:Астро"],
        "# Астро\n\nКвазар светит ярко.\n".as_bytes(),
    )));
    let s = "hc-1";
    p.hook(
        "session-start",
        &p.ev("SessionStart", s, json!({"source": "startup"})),
    );
    let read = || {
        let o = p
            .cmd(&p.url)
            .env("ZAM_SESSION", s)
            .args(["read", "--batch", &format!("n:{n}")])
            .output()
            .unwrap();
        ok(&o)
    };
    let first = read();
    assert!(
        first.contains("Квазар светит ярко.") && !first.contains(REREAD),
        "{first}"
    );
    let again = read();
    assert!(
        again.contains("Квазар светит ярко.") && again.contains(REREAD),
        "{again}"
    );
    let pc = p.ev(
        "PreCompact",
        s,
        json!({"trigger": "auto", "custom_instructions": ""}),
    );
    // промахи считаются только у двух вызовов ниже (не у `startup` до чтения)
    let log_before = p.log();
    empty(&p.hook("checkpoint", &pc));
    p.hook(
        "session-start",
        &p.ev("SessionStart", s, json!({"source": "compact"})),
    );
    // обе новые эпохи; без промахов — ровно 2. Промах бюджета (отладочная сборка на занятом
    // HDD) или сбой запроса этих двух вызовов допускает меньше, но не 0
    let after = p.log();
    let fresh = after.strip_prefix(log_before.as_str()).unwrap_or(&after);
    let missed = fresh.matches(zam_core::HOOK_BUDGET_SPENT).count()
        + fresh.matches("sessions(new_epoch)").count()
        + fresh.matches("recall(session_start)").count();
    let epoch = p.session(s)["seen_epoch"].as_u64().unwrap() as usize;
    if missed == 0 {
        assert_eq!(epoch, 2, "{fresh}");
    } else {
        assert!(
            (1..=2).contains(&epoch),
            "эпоха {epoch}, промахов {missed}: {fresh}"
        );
    }
    let after = read();
    assert!(
        after.contains("Квазар светит ярко.") && !after.contains(REREAD),
        "{after}"
    );
    assert!(read().contains(REREAD));
}

// ---- ревью задачи 8a M6 (C1): предел канала агента укладывает сервер ----

/// Пометка клиентской обрезки (`hook::output::CUT_MARK`).
const CUT_MARK: &str = "(подсказки ZeroAIMemory обрезаны)";

/// `POST <path>` токеном проекта; тело ответа.
fn api_post(p: &Proj, path: &str, body: Value) -> Value {
    let r = http()
        .post(&format!("{}/api/v1{path}", p.url))
        .header("Authorization", format!("Bearer {}", p.token))
        .content_type("application/json")
        .send(body.to_string())
        .unwrap();
    let st = r.status().as_u16();
    let text = r.into_body().read_to_string().unwrap();
    assert!((200..300).contains(&st), "{path}: {st}: {text}");
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// Мир «20 правил × 200 токенов кириллицей»: решение, ловушки, карта кода; id правил.
fn heavy_rules(p: &Proj) -> Vec<String> {
    api_post(
        p,
        "/solutions",
        json!({"agent_solution_id": G, "name": "Хуки"}),
    );
    let mut ids = Vec::new();
    let dirs = ["security", "process", "backend", "frontend", "infra"];
    for i in 0..20 {
        let body: String = (0..12)
            .map(|k| {
                format!(
                    "## Пункт {k}\n\nПредложение {k} правила {i} требует проверки изменений \
                     и записи в журнал.\n\n"
                )
            })
            .collect();
        let v = api_post(
            p,
            "/nodes",
            json!({"space": "rules", "path": format!("{}/правило-{i:02}", dirs[i % 5]),
                   "kind": "note",
                   "content": format!("---\nl0: Кратко о правиле {i}\n---\n# Правило {i}\n\n{body}\n")}),
        );
        ids.push(format!("n:{}", v["id"].as_str().unwrap()));
    }
    for i in 0..3 {
        p.insight(&format!("Ловушка сборки {i}"), "gotcha", "solution", &[]);
    }
    let h = format!("{:064x}", 1);
    api_post(
        p,
        "/code/sync",
        json!({"solution": G, "commit": "abcdef1234", "branch": "main", "machine": "box",
               "manifest": [{"path": "src/a.rs", "hash": h}]}),
    );
    let r = http()
        .put(&format!("{}/api/v1/code/files", p.url))
        .header("Authorization", format!("Bearer {}", p.token))
        .content_type("application/json")
        .send(
            json!({"solution": G, "commit": "abcdef1234", "branch": "main", "machine": "box",
                   "files": [{"path": "src/a.rs", "lang": "rust", "hash": h, "size": 10,
                              "parse_state": "ok", "refs": [],
                              "symbols": [{"name": "alpha", "qual": "alpha", "kind": "function",
                                           "line": 1, "end_line": 3, "sig": "fn alpha()",
                                           "body_hash": "0000000000000001", "exported": true}]}]})
            .to_string(),
        )
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    ids
}

/// `zam hook session-start --agent <agent>` со щедрым бюджетом (`ZAM_TEST_HOOK_BUDGET_MS`,
/// отладочная сборка): тест проверяет содержимое вывода, а не скорость (повторное ревью 8a).
fn hook_roomy(p: &Proj, agent: &str, ev: &Value) -> Output {
    p.cmd(&p.url)
        .env("ZAM_TEST_HOOK_BUDGET_MS", "10000")
        .args(["hook", "session-start", "--agent", agent])
        .write_stdin(ev.to_string().into_bytes())
        .output()
        .unwrap()
}

/// Ревью 8a M6, C1: `session-start` Claude Code и Codex на мире «20 правил × 200 токенов
/// кириллицей» — вывод агенту (`Output::render`) без клиентской обрезки: все правила (хвост
/// приоритета — L0), строка карты кода и «Ловушки» на месте; после `compact` — то же вместе со
/// сводкой журнала (`with_tail`); в `hooks.log` нет записи об обрезке.
#[test]
fn session_start_fits_channel_codex_and_claude() {
    let _g = serial();
    let (url, tok) = start_opt("fit-channel", true);
    let p = Proj::new("fit-channel", &url, &tok);
    let ids = heavy_rules(&p);
    for (agent, max) in [("codex", 7500usize), ("claude", 9000)] {
        let size = |t: &str| {
            if agent == "codex" {
                t.len()
            } else {
                t.chars().count()
            }
        };
        let sid = format!("fit-{agent}");
        let ss = |src: &str| p.ev("SessionStart", &sid, json!({"source": src}));
        let o = hook_roomy(&p, agent, &ss("startup"));
        let c = context(&o, "SessionStart");
        assert!(size(&c) <= max, "{agent}: {} > {max}", size(&c));
        assert!(!c.contains(CUT_MARK), "{agent}: обрезка клиентом: {c}");
        for r in &ids {
            assert!(c.contains(r.as_str()), "{agent}: нет {r}: {c}");
        }
        assert!(c.contains("ZeroAIMemory: карта кода:"), "{agent}: {c}");
        assert!(c.contains("### Ловушки и сбои"), "{agent}: {c}");
        // compact: пакет восстановления и сводка журнала — в общем пределе
        fix_pair(&p, &sid);
        let o = hook_roomy(&p, agent, &ss("compact"));
        let c = context(&o, "SessionStart");
        assert!(size(&c) <= max, "{agent} compact: {} > {max}", size(&c));
        assert!(
            !c.contains(CUT_MARK),
            "{agent} compact: обрезка клиентом: {c}"
        );
        assert!(c.contains("До сжатия в этой сессии"), "{agent}: {c}");
        for r in &ids {
            assert!(c.contains(r.as_str()), "{agent} compact: нет {r}: {c}");
        }
    }
    assert!(!p.log().contains("обрезан клиентом"), "{}", p.log());
}

// ---- M6 §12 (задача 12): влияние диффа в `checkpoint`, контрольная группа `holdout` ----

/// Строка влияния правки в напоминании `Stop` (§12).
const IMPACT_MARK: &str = "правка затрагивает";

/// Хук `kind` с щедрым бюджетом процесса и сроком `POST /code/impact` (отладочная сборка под
/// нагрузкой; проверка содержимого, а не времени).
fn hook_slow(p: &Proj, url: &str, kind: &str, ev: &Value) -> Output {
    p.cmd(url)
        .env("ZAM_TEST_HOOK_BUDGET_MS", "10000")
        .env("ZAM_TEST_HOOK_IMPACT_MS", "10000")
        .args(["hook", kind, "--agent", "claude"])
        .write_stdin(ev.to_string().into_bytes())
        .output()
        .unwrap()
}

/// `[hooks]` проекта (`<repo>/.zeroaimemory/config.toml`).
fn project_hooks(p: &Proj, body: &str) {
    std::fs::write(
        p.repo.join(".zeroaimemory/config.toml"),
        format!("[hooks]\n{body}\n"),
    )
    .unwrap();
}

/// §12: `hooks.checkpoint_impact = true` — до 3 строк «правка затрагивает `<путь>` → i:<id> «…»»
/// **внутри** единственного напоминания `Stop` (одно `decision: block`, второго нет); сами строки
/// напоминания не вызывают (в память уже писали — тишина); ключ выключен — строк нет; сессия
/// контрольной группы (`holdout` в `GET /sessions/{id}`) — тишина целиком (маркер).
#[test]
fn checkpoint_impact_lines_inside_single_reminder() {
    let _g = serial();
    let (url, tok) = start("cp-impact");
    let p = Proj::new("cp-impact", &url, &tok);
    std::fs::create_dir_all(p.repo.join("src")).unwrap();
    std::fs::write(p.repo.join("src/pool.rs"), b"pub fn pool() {}\n").unwrap();
    let mut ids = Vec::new();
    for t in [
        "Пул течёт при панике",
        "Пул не закрывается",
        "Пул и таймауты",
        "Пул: четвёртое",
    ] {
        let o = p.zam(
            &[
                "log",
                "insight",
                "--kind",
                "gotcha",
                "--title",
                t,
                "--ref",
                "src/pool.rs",
            ],
            "Разобрано.\n".as_bytes(),
        );
        ids.push(format!("i:{}", id_of(&ok(&o))));
    }
    let edit = |s: &str| {
        std::fs::write(
            p.repo.join("src/pool.rs"),
            format!("pub fn pool() {{}} // {s}\n"),
        )
        .unwrap();
        let ev = post_ok(
            &p,
            s,
            "Edit",
            json!({"file_path": p.repo.join("src/pool.rs"), "old_string": "a", "new_string": "b"}),
        );
        ok(&hook_slow(&p, &url, "post-tool", &ev));
    };
    let stop = |s: &str| {
        let ev = p.ev("Stop", s, json!({"stop_hook_active": false}));
        hook_json(&hook_slow(&p, &url, "checkpoint", &ev))
    };
    let start_s = |s: &str| {
        let ev = p.ev("SessionStart", s, json!({"source": "startup"}));
        ok(&hook_slow(&p, &url, "session-start", &ev));
    };
    // ключ включён: строки — внутри единственного напоминания
    project_hooks(&p, "checkpoint_impact = true");
    start_s("ci-1");
    edit("ci-1");
    let v = stop("ci-1");
    assert_eq!(v["decision"], "block", "{v}\n{}", p.log());
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("zam log changelog"), "{reason}");
    let lines: Vec<&str> = reason.lines().filter(|l| l.contains(IMPACT_MARK)).collect();
    assert_eq!(lines.len(), 3, "до 3 строк: {reason}\n{}", p.log());
    for l in &lines {
        assert!(
            l.starts_with("ZeroAIMemory: правка затрагивает `src/pool.rs`"),
            "{l}"
        );
        assert!(ids.iter().any(|i| l.contains(&format!("→ {i} «"))), "{l}");
    }
    assert_eq!(stop("ci-1"), Value::Null, "второго напоминания нет");
    // в память уже писали: строки влияния напоминания не вызывают
    start_s("ci-2");
    edit("ci-2");
    let o = p
        .cmd(&p.url)
        .env("CLAUDECODE", "1")
        .args(["log", "changelog", "--kind", "fix", "--title", "Итог"])
        .write_stdin("Сделано.\n")
        .output()
        .unwrap();
    ok(&o);
    assert_eq!(stop("ci-2"), Value::Null, "{}", p.log());
    // контрольная группа (ревью задачи 12, I4): ответ `GET /sessions/{id}` с `holdout` ставит
    // маркер — ни строк влияния, ни самого напоминания
    let r = http()
        .post(&format!("{url}/api/v1/sessions"))
        .header("Authorization", format!("Bearer {tok}"))
        .content_type("application/json")
        .send(r#"{"id":"ci-3","holdout":true}"#)
        .unwrap();
    assert_eq!(r.status(), 201);
    edit("ci-3");
    assert_eq!(stop("ci-3"), Value::Null, "{}", p.log());
    assert!(p.session_file("holdout", "ci-3", "").is_some());
    // ключ выключен (по умолчанию): строк нет
    project_hooks(&p, "");
    edit("ci-4");
    let v = stop("ci-4");
    assert_eq!(v["decision"], "block", "{v}");
    assert!(!v["reason"].as_str().unwrap().contains(IMPACT_MARK), "{v}");
    let log = p.log();
    assert!(!log.contains("code/impact"), "{log}");
}

/// §12: `POST /code/impact` — со своим сроком 60 мс внутри бюджета `checkpoint` (100 мс) и
/// параллельно с `GET /sessions/{id}`: сервер влияния не ответил — напоминание без строк, хук
/// его не ждёт; причина — в `hooks.log`. Тело запроса — решение и путь правки без символов.
#[test]
fn checkpoint_impact_timeout_60ms_no_lines() {
    let _g = serial();
    let knowledge = r#"{"changed":[],"removed":[],"consumers":[],"paths_only":["src/pool.rs"],"knowledge_left":0,"knowledge":[{"path":"src/pool.rs","id":"i:01J0000000000000000000000","kind":"insight","title":"Пул течёт","state":"ok","space":null}],"warnings":[],"tokens":10}"#;
    let (url, seen) = common::stub_body(std::sync::Arc::new(
        move |m: &str, path: &str, _: usize, _: &Value| {
            if path.starts_with("/api/v1/code/impact") {
                (200, knowledge.to_string(), Duration::from_millis(1500))
            } else if m == "GET" && path.starts_with("/api/v1/sessions/") {
                (
                    200,
                    r#"{"memory_writes":0,"claimed_tasks":[]}"#.to_string(),
                    Duration::ZERO,
                )
            } else {
                (200, "{}".to_string(), Duration::ZERO)
            }
        },
    ));
    let p = Proj::new("cp-impact-slow", &url, "zam_x");
    project_hooks(&p, "checkpoint_impact = true");
    std::fs::create_dir_all(p.repo.join("src")).unwrap();
    std::fs::write(p.repo.join("src/pool.rs"), b"pub fn pool() {}\n").unwrap();
    let ev = post_ok(
        &p,
        "cs-1",
        "Edit",
        json!({"file_path": p.repo.join("src/pool.rs"), "old_string": "a", "new_string": "b"}),
    );
    ok(&p
        .hook_at(DEAD, "post-tool", "claude", ev.to_string().as_bytes())
        .0);
    // щедрый бюджет процесса — но не срок влияния (60 мс по умолчанию)
    let stop = p.ev("Stop", "cs-1", json!({"stop_hook_active": false}));
    let mut c = p.cmd(&url);
    c.env("ZAM_TEST_HOOK_BUDGET_MS", "10000")
        .args(["hook", "checkpoint", "--agent", "claude"])
        .write_stdin(stop.to_string().into_bytes());
    let t = Instant::now();
    let o = c.output().unwrap();
    let d = t.elapsed();
    let v = hook_json(&o);
    assert_eq!(v["decision"], "block", "{v}\n{}", p.log());
    assert!(!v["reason"].as_str().unwrap().contains(IMPACT_MARK), "{v}");
    assert!(
        d < Duration::from_millis(1000),
        "хук ждал сервер влияния: {d:?}"
    );
    let sent = seen.lock().unwrap().clone();
    let imp: Vec<&Value> = sent
        .iter()
        .filter(|(m, path, _)| m == "POST" && path == "/api/v1/code/impact")
        .map(|(_, _, b)| b)
        .collect();
    assert_eq!(imp.len(), 1, "{sent:?}");
    assert_eq!(imp[0]["solution"], G);
    assert_eq!(
        imp[0]["files"],
        json!([{"path": "src/pool.rs"}]),
        "{sent:?}"
    );
    assert!(p.log().contains("code/impact"), "{}", p.log());
    // журнал выключен (ревью задачи 12, M5): путь — из очереди изменённых, прочитанной потоком
    // влияния до её отправки тем же `checkpoint`
    project_hooks(
        &p,
        "checkpoint_impact = true
session_journal = false",
    );
    let ev = post_ok(
        &p,
        "cs-2",
        "Edit",
        json!({"file_path": p.repo.join("src/pool.rs"), "old_string": "a", "new_string": "b"}),
    );
    ok(&p
        .hook_at(DEAD, "post-tool", "claude", ev.to_string().as_bytes())
        .0);
    assert_eq!(queue(&p, "cs-2").len(), 1);
    let stop = p.ev("Stop", "cs-2", json!({"stop_hook_active": false}));
    let mut c = p.cmd(&url);
    c.env("ZAM_TEST_HOOK_BUDGET_MS", "10000")
        .args(["hook", "checkpoint", "--agent", "claude"])
        .write_stdin(stop.to_string().into_bytes());
    assert_eq!(hook_json(&c.output().unwrap())["decision"], "block");
    let sent = seen.lock().unwrap().clone();
    let imp: Vec<&Value> = sent
        .iter()
        .filter(|(m, path, _)| m == "POST" && path == "/api/v1/code/impact")
        .map(|(_, _, b)| b)
        .collect();
    assert_eq!(imp.len(), 2, "{sent:?}");
    assert_eq!(
        imp[1]["files"],
        json!([{"path": "src/pool.rs"}]),
        "{sent:?}"
    );
    assert!(
        sent.iter()
            .any(|(m, path, _)| m == "POST" && path == "/api/v1/files/changed"),
        "очередь отправлена: {sent:?}"
    );
}

/// §12: `hooks.holdout = p` — `session-start` ставит сессии «тихий» режим (решение по хэшу id,
/// вероятность p) отдельным `POST /sessions {id, holdout: true}` **до** `/recall`: дайджест
/// пустой с первого старта, после compact — тоже (метка на сервере); остальные сессии — как
/// обычно; `0` — ни одной; вне 0..=0,5 — строка в журнале и ни одной.
#[test]
fn session_start_holdout_quiet_group() {
    let _g = serial();
    let (url, tok) = start("ss-holdout");
    let p = Proj::new("ss-holdout", &url, &tok);
    ok(&p.zam(
        &[
            "log",
            "insight",
            "--kind",
            "gotcha",
            "--title",
            "LF в скриптах",
            "--scope",
            "global",
        ],
        "CRLF ломает shebang.\n".as_bytes(),
    ));
    let ss = |s: &str, src: &str| {
        let ev = p.ev("SessionStart", s, json!({"source": src}));
        hook_slow(&p, &url, "session-start", &ev)
    };
    project_hooks(&p, "holdout = 0.5");
    let (mut quiet, mut loud) = (Vec::new(), Vec::new());
    for i in 0..16 {
        let s = format!("hd-{i}");
        let o = ss(&s, "startup");
        let held = p.session(&s)["holdout"] == true;
        // группа — та, что даёт `holdout_pick` хука (ревью задачи 12, M7)
        assert_eq!(held, common::holdout_pick(&s, 0.5), "{s}");
        if held {
            empty(&o);
            quiet.push(s);
        } else {
            assert!(context(&o, "SessionStart").contains("LF в скриптах"), "{s}");
            loud.push(s);
        }
    }
    assert!(
        !quiet.is_empty() && !loud.is_empty(),
        "q={quiet:?} l={loud:?}"
    );
    // решение детерминировано: повторный старт той же сессии — та же группа
    empty(&ss(&quiet[0], "compact"));
    assert_eq!(p.session(&quiet[0])["holdout"], true);
    assert!(!out(&ss(&loud[0], "resume")).trim().is_empty());
    assert_eq!(p.session(&loud[0])["holdout"], false);
    // метка на сервере переживает выключение ключа (та же сессия после compact); новые
    // сессии — без неё
    project_hooks(&p, "");
    empty(&ss(&quiet[0], "compact"));
    for s in &quiet {
        let id = format!("{s}-new");
        assert!(!out(&ss(&id, "startup")).trim().is_empty());
        assert_eq!(p.session(&id)["holdout"], false);
    }
    // вне диапазона — строка в журнале, контрольной группы нет
    project_hooks(&p, "holdout = 0.9");
    for s in &quiet {
        let id = format!("{s}-bad");
        assert!(!out(&ss(&id, "startup")).trim().is_empty());
        assert_eq!(p.session(&id)["holdout"], false);
    }
    assert!(p.log().contains("hooks.holdout"), "{}", p.log());
}

/// §12, §13: старый сервер (v0.0.10-m5.7) отвечает на `POST /sessions {id, holdout}` 400
/// (`deny_unknown_fields`) — хук молча продолжает: дайджест выводится, в `hooks.log` о нём ничего;
/// запрос — отдельный, только `{id, holdout: true}`, и уходит до `/recall`.
#[test]
fn session_start_holdout_old_server_400_ignored() {
    let _g = serial();
    let (url, seen) = common::stub_body(std::sync::Arc::new(
        |m: &str, path: &str, _: usize, b: &Value| {
            let reply = |code: u16, body: &str| (code, body.to_string(), Duration::ZERO);
            if m == "POST" && path == "/api/v1/sessions" && b.get("holdout").is_some() {
                reply(
                    400,
                    r#"{"type":"/problems/bad-request","status":400,"detail":"некорректное тело: unknown field `holdout`, expected one of `id`, `agent`"}"#,
                )
            } else if path == "/api/v1/sessions" {
                reply(201, r#"{"id":"x"}"#)
            } else if path == "/api/v1/recall" {
                reply(
                    200,
                    r#"{"text":"дайджест старого сервера","items":[],"tokens":3,"warnings":[]}"#,
                )
            } else {
                reply(200, "{}")
            }
        },
    ));
    let p = Proj::new("ss-holdout-old", &url, "zam_x");
    project_hooks(&p, "holdout = 0.5");
    let mut asked = 0;
    for i in 0..12 {
        let s = format!("ho-{i}");
        let ev = p.ev("SessionStart", &s, json!({"source": "startup"}));
        let o = hook_slow(&p, &url, "session-start", &ev);
        assert!(
            context(&o, "SessionStart").contains("дайджест старого сервера"),
            "{s}"
        );
        let sent = seen.lock().unwrap().clone();
        let hold = sent.iter().position(|(m, path, b)| {
            m == "POST"
                && path == "/api/v1/sessions"
                && b["id"] == s.as_str()
                && b.get("holdout").is_some()
        });
        if let Some(h) = hold {
            asked += 1;
            assert_eq!(sent[h].2, json!({"id": s, "holdout": true}), "{sent:?}");
            let recall = sent
                .iter()
                .position(|(_, path, b)| path == "/api/v1/recall" && b["session_id"] == s.as_str())
                .expect("recall");
            assert!(h < recall, "holdout — до /recall: {sent:?}");
        }
    }
    assert!(asked > 0, "ни одной контрольной сессии из 12");
    let log = p.log();
    assert!(
        !log.contains("holdout"),
        "400 старого сервера — молча: {log}"
    );
}

/// Копия `holdout_pick` в `tests/common` совпадает с функцией хука: те же закреплённые значения,
/// что в юнит-тесте `holdout_pick_share_and_determinism` (ревью задачи 12, M7).
#[test]
fn holdout_pick_copy_pinned() {
    let pinned: Vec<bool> = (0..8)
        .map(|i| common::holdout_pick(&format!("pin-{i}"), 0.5))
        .collect();
    assert_eq!(pinned, PINNED_HOLDOUT, "{pinned:?}");
}

/// `holdout_pick("pin-0".."pin-7", 0.5)` — как `PINNED_HOLDOUT` юнит-теста `session_start`.
const PINNED_HOLDOUT: [bool; 8] = [false, true, true, true, false, true, true, false];

/// Ревью задачи 12, I1: в сессии контрольной группы сервер отдаёт `items: []`, и локальные
/// совпадения `error_regex` кэша триггеров не должны выйти агенту — ответ с `holdout: true`
/// гасит и их, и строку петли; маркер сессии ставится. Обычная сессия — локальная подсказка.
#[test]
fn post_tool_error_regex_silent_in_holdout() {
    let _g = serial();
    let (url, tok) = start("pt-holdout");
    let p = Proj::new("pt-holdout", &url, &tok);
    write_cache(&p, &url);
    let r = http()
        .post(&format!("{url}/api/v1/sessions"))
        .header("Authorization", format!("Bearer {tok}"))
        .content_type("application/json")
        .send(r#"{"id":"ph-1","holdout":true}"#)
        .unwrap();
    assert_eq!(r.status(), 201);
    let ev = |s: &str| {
        p.ev(
            "PostToolUseFailure",
            s,
            json!({"tool_name": "Bash", "tool_input": {"command": "./deploy.sh"},
                   "error": "/bin/bash^M: bad interpreter: No such file or directory"}),
        )
    };
    // обычная сессия: локальное совпадение `error_regex` из кэша
    let c = context(
        &hook_slow(&p, &url, "post-tool", &ev("ph-0")),
        "PostToolUseFailure",
    );
    assert!(c.contains("CRLF в shebang [gotcha·th] i:E3"), "{c}");
    // контрольная группа: тишина — и при третьем повторе (строка петли)
    assert!(p.session_file("holdout", "ph-1", "").is_none());
    for _ in 0..3 {
        empty(&hook_slow(&p, &url, "post-tool", &ev("ph-1")));
    }
    assert!(
        p.session_file("holdout", "ph-1", "").is_some(),
        "маркер контрольной группы"
    );
    assert_eq!(
        events(&p, "ph-1")
            .iter()
            .filter(|e| e["ev"] == "error")
            .count(),
        3,
        "журнал ведётся"
    );
}

/// Ревью задачи 12, I4: сессия контрольной группы (маркер `holdout-<сессия>` от `session-start`)
/// молчит во **всех** хуках, включая локальный вывод без сети: `pre-tool` (триггер `file_glob`),
/// `post-tool` `Read` (знание о файле), `checkpoint` (напоминание `Stop`); учёта локального
/// вывода нет (очереди `tokens-…` нет), журнал сессии ведётся. Сессия вне группы — всё как
/// обычно.
#[test]
fn holdout_marker_silences_all_hooks() {
    let _g = serial();
    let (url, tok) = start("holdout-marker");
    let p = Proj::new("holdout-marker", &url, &tok);
    project_hooks(&p, "holdout = 0.5");
    let quiet = common::holdout_session("hm", 0.5, true);
    let loud = common::holdout_session("hm", 0.5, false);
    for s in [&quiet, &loud] {
        let ev = p.ev("SessionStart", s, json!({"source": "startup"}));
        ok(&hook_slow(&p, &url, "session-start", &ev));
    }
    assert!(p.session_file("holdout", &quiet, "").is_some());
    assert!(p.session_file("holdout", &loud, "").is_none());
    // кэши — после старта (старт перезаписывает их ответом сервера)
    write_cache(&p, &url);
    write_file_refs(&p, &url);
    std::fs::create_dir_all(p.repo.join("src")).unwrap();
    std::fs::write(p.repo.join("src/pool.rs"), b"pub fn pool() {}\n").unwrap();
    let pre = |s: &str| hook_slow(&p, &url, "pre-tool", &write_ev(&p, s, "deploy.sh"));
    let read = |s: &str| {
        let ev = post_ok(
            &p,
            s,
            "Read",
            json!({"file_path": p.repo.join("src/pool.rs")}),
        );
        hook_slow(&p, &url, "post-tool", &ev)
    };
    let stop = |s: &str| {
        let ev = p.ev("Stop", s, json!({"stop_hook_active": false}));
        hook_slow(&p, &url, "checkpoint", &ev)
    };
    // вне группы — подсказки; в группе — тишина
    assert!(context(&pre(&loud), "PreToolUse").contains("Скрипты — только LF"));
    assert!(context(&read(&loud), "PostToolUse").contains("Пул соединений течёт"));
    empty(&pre(&quiet));
    empty(&read(&quiet));
    // учёт локального вывода — только у вывода, который был (до отправки очереди `checkpoint`)
    assert!(p.session_file("tokens", &loud, ".jsonl").is_some());
    assert!(
        p.session_file("tokens", &quiet, ".jsonl").is_none(),
        "локальный вывод не учитывается"
    );
    assert_eq!(hook_json(&stop(&loud))["decision"], "block");
    empty(&stop(&quiet));
    let ev = events(&p, &quiet);
    assert!(
        ev.iter().any(|e| e["ev"] == "read"),
        "журнал ведётся: {ev:?}"
    );
}

/// Ревью задачи 12, I2: метка `POST /sessions {holdout}` и `/recall` старта не складывают свои
/// сроки — у метки свой срок 100 мс, `/recall` — новым клиентом по остатку бюджета 300 мс.
/// Заглушка не отвечает ни на то, ни на другое: хук укладывается в бюджет с запасом на запуск
/// процесса, а не в сумму двух сроков (~540 мс).
#[test]
fn session_start_holdout_within_budget() {
    let _g = serial();
    let (url, seen) = common::stub_body(std::sync::Arc::new(
        |m: &str, path: &str, _: usize, b: &Value| {
            let slow = (m == "POST" && path == "/api/v1/sessions" && b.get("holdout").is_some())
                || path == "/api/v1/recall";
            let delay = if slow {
                Duration::from_millis(1500)
            } else {
                Duration::ZERO
            };
            (200, r#"{"id":"x"}"#.to_string(), delay)
        },
    ));
    let p = Proj::new("ss-holdout-budget", &url, "zam_x");
    project_hooks(&p, "holdout = 0.5");
    let s = common::holdout_session("hb", 0.5, true);
    let ev = p.ev("SessionStart", &s, json!({"source": "startup"}));
    // прогрев процесса (первый запуск отладочного бинарника — с диска)
    let _ = p.hook_at(&url, "session-start", "claude", ev.to_string().as_bytes());
    let mut times = Vec::new();
    for _ in 0..5 {
        let (o, d) = p.hook_at(&url, "session-start", "claude", ev.to_string().as_bytes());
        ok(&o);
        times.push(d);
    }
    let best = *times.iter().min().unwrap();
    assert!(
        best < Duration::from_millis(450),
        "session-start {times:?}: метка и /recall сложили сроки"
    );
    let sent = seen.lock().unwrap().clone();
    assert!(
        sent.iter()
            .any(|(_, path, b)| path == "/api/v1/sessions" && b.get("holdout").is_some()),
        "{sent:?}"
    );
}

/// Повторное ревью задачи 12, m1: метка контрольной группы — только на первом старте сессии.
/// Метка не успела (сервер не ответил в срок) — сессия вне группы: дайджест выводится, причина —
/// в `hooks.log`; повторный старт (compact) метку не шлёт и тихой сессию не делает.
#[test]
fn session_start_holdout_only_on_first_start() {
    let _g = serial();
    let (url, seen) = common::stub_body(std::sync::Arc::new(
        |m: &str, path: &str, _: usize, b: &Value| {
            if m == "POST" && path == "/api/v1/sessions" && b.get("holdout").is_some() {
                (
                    201,
                    r#"{"id":"x","holdout":true}"#.to_string(),
                    Duration::from_millis(1500),
                )
            } else if path == "/api/v1/recall" {
                (
                    200,
                    r#"{"text":"дайджест","items":[],"tokens":1,"warnings":[]}"#.to_string(),
                    Duration::ZERO,
                )
            } else {
                (200, r#"{"id":"x"}"#.to_string(), Duration::ZERO)
            }
        },
    ));
    let p = Proj::new("ss-holdout-first", &url, "zam_x");
    project_hooks(&p, "holdout = 0.5");
    let s = common::holdout_session("hf", 0.5, true);
    let start = |src: &str| {
        let ev = p.ev("SessionStart", &s, json!({"source": src}));
        // щедрый бюджет процесса; у метки — свой срок 100 мс, она его не дождётся
        hook_slow(&p, &url, "session-start", &ev)
    };
    let holds = || {
        seen.lock()
            .unwrap()
            .iter()
            .filter(|(m, path, b)| {
                m == "POST" && path == "/api/v1/sessions" && b.get("holdout").is_some()
            })
            .count()
    };
    assert!(context(&start("startup"), "SessionStart").contains("дайджест"));
    assert_eq!(holds(), 1);
    assert!(p.log().contains("sessions(holdout)"), "{}", p.log());
    for src in ["compact", "resume"] {
        assert!(
            context(&start(src), "SessionStart").contains("дайджест"),
            "{src}"
        );
        assert_eq!(holds(), 1, "{src}: метка повторно не шлётся");
        assert!(p.session_file("holdout", &s, "").is_none(), "{src}");
    }
}

/// Повторное ревью задачи 12, m2: метку на сервере сняли (`holdout: false`) — первый же успешный
/// ответ без неё (`post-tool`, `prompt`, `checkpoint`) снимает маркер, и хуки снова говорят.
#[test]
fn holdout_marker_cleared_by_server_response() {
    let _g = serial();
    let (url, tok) = start("holdout-clear");
    let p = Proj::new("holdout-clear", &url, &tok);
    write_cache(&p, &url);
    let post = |body: &str| {
        let r = http()
            .post(&format!("{url}/api/v1/sessions"))
            .header("Authorization", format!("Bearer {tok}"))
            .content_type("application/json")
            .send(body)
            .unwrap();
        assert_eq!(r.status(), 201);
    };
    let fail = p.ev(
        "PostToolUseFailure",
        "hc-1",
        json!({"tool_name": "Bash", "tool_input": {"command": "./deploy.sh"},
               "error": "/bin/bash^M: bad interpreter: No such file or directory"}),
    );
    post(r#"{"id":"hc-1","holdout":true}"#);
    empty(&hook_slow(&p, &url, "post-tool", &fail));
    assert!(p.session_file("holdout", "hc-1", "").is_some());
    // метку сняли — ответ `prompt` без неё снимает маркер
    post(r#"{"id":"hc-1","holdout":false}"#);
    let pr = p.ev("UserPromptSubmit", "hc-1", json!({"prompt": "что-нибудь"}));
    ok(&hook_slow(&p, &url, "prompt", &pr));
    assert!(p.session_file("holdout", "hc-1", "").is_none());
    let c = context(
        &hook_slow(&p, &url, "post-tool", &fail),
        "PostToolUseFailure",
    );
    assert!(c.contains("CRLF в shebang"), "{c}");
    // и снова: метка — маркер (post-tool), снятие — по ответу `post-tool`
    post(r#"{"id":"hc-1","holdout":true}"#);
    empty(&hook_slow(&p, &url, "post-tool", &fail));
    assert!(p.session_file("holdout", "hc-1", "").is_some());
    post(r#"{"id":"hc-1","holdout":false}"#);
    assert!(
        !out(&hook_slow(&p, &url, "post-tool", &fail))
            .trim()
            .is_empty()
    );
    assert!(p.session_file("holdout", "hc-1", "").is_none());
}

// ---- M6 §8а (задача 17b): снимок сессии в хуках, очередь, старый сервер, выключатель ----

/// Запись журнала Claude Code: запрос человека сессии `s`.
fn cl_user(s: &str, text: &str) -> String {
    json!({"type": "user", "isSidechain": false, "sessionId": s,
           "message": {"role": "user", "content": text}})
    .to_string()
}

/// Запись журнала Claude Code: вызов `Bash` (`id` — id вызова).
fn cl_bash(s: &str, id: &str, command: &str) -> String {
    json!({"type": "assistant", "isSidechain": false, "sessionId": s,
           "message": {"role": "assistant", "content": [
               {"type": "tool_use", "id": id, "name": "Bash", "input": {"command": command}}]}})
    .to_string()
}

/// Запись журнала Claude Code: правка файла `path`.
fn cl_edit(s: &str, id: &str, path: &std::path::Path) -> String {
    json!({"type": "assistant", "isSidechain": false, "sessionId": s,
           "message": {"role": "assistant", "content": [
               {"type": "tool_use", "id": id, "name": "Edit",
                "input": {"file_path": path, "old_string": "a", "new_string": "b"}}]}})
    .to_string()
}

/// Строка журнала без признаков предфильтра (вывод агента): `n` байт.
fn cl_filler(n: usize) -> String {
    json!({"type": "progress", "data": "ж".repeat(n / 2)}).to_string()
}

/// Журнал агента сессии вне корня проекта (как `~/.claude/projects/…`).
fn transcript_path(p: &Proj, name: &str) -> PathBuf {
    let d = p.home.join(".claude/projects/proj");
    std::fs::create_dir_all(&d).unwrap();
    d.join(format!("{name}.jsonl"))
}

fn write_lines(path: &std::path::Path, lines: &[String]) {
    std::fs::write(
        path,
        lines.iter().map(|l| format!("{l}\n")).collect::<String>(),
    )
    .unwrap();
}

fn append_lines(path: &std::path::Path, lines: &[String]) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    for l in lines {
        writeln!(f, "{l}").unwrap();
    }
}

/// Событие хука с журналом агента `transcript`.
fn snap_ev(p: &Proj, event: &str, session: &str, t: &std::path::Path, extra: Value) -> Value {
    let mut v = p.ev(event, session, extra);
    v["transcript_path"] = json!(t);
    v
}

/// Хук со щедрым бюджетом и сроком снимка (проверка содержимого, а не скорости).
fn hook_snap(p: &Proj, kind: &str, ev: &Value) -> Output {
    p.cmd(&p.url)
        .env("ZAM_TEST_HOOK_BUDGET_MS", "10000")
        .env("ZAM_TEST_SNAPSHOT_MS", "5000")
        .args(["hook", kind, "--agent", "claude"])
        .write_stdin(ev.to_string().into_bytes())
        .output()
        .unwrap()
}

/// Заглушка сервера снимка: `PUT …/snapshot` — `put` (статус и тело); `GET /sessions/{id}` —
/// сессия без записей; `/recall` — дайджест «D»; прочее — 200 `{}`.
fn snap_stub(put: u16, put_body: &'static str) -> (String, common::Seen) {
    common::stub_body(std::sync::Arc::new(
        move |m: &str, path: &str, _: usize, _: &Value| {
            let ok = |b: &str| (200u16, b.to_string(), Duration::ZERO);
            if m == "PUT" && path.ends_with("/snapshot") {
                (put, put_body.to_string(), Duration::ZERO)
            } else if m == "GET" && path.starts_with("/api/v1/sessions/") {
                ok(r#"{"memory_writes":0,"claimed_tasks":[]}"#)
            } else if path == "/api/v1/recall" {
                ok(r#"{"text":"D"}"#)
            } else {
                ok("{}")
            }
        },
    ))
}

/// Принятые заглушкой `PUT …/snapshot`: путь и тело.
fn puts(seen: &common::Seen) -> Vec<(String, Value)> {
    seen.lock()
        .unwrap()
        .iter()
        .filter(|(m, p, _)| m == "PUT" && p.ends_with("/snapshot"))
        .map(|(_, p, b)| (p.clone(), b.clone()))
        .collect()
}

/// Номер первого запроса `(метод, путь)` среди принятых заглушкой, начиная с `from`.
fn req_index(
    seen: &common::Seen,
    from: usize,
    m: &str,
    pred: impl Fn(&str) -> bool,
) -> Option<usize> {
    seen.lock()
        .unwrap()
        .iter()
        .skip(from)
        .position(|(x, p, _)| x == m && pred(p))
}

fn snap_file(p: &Proj, prefix: &str, session: &str) -> Option<Value> {
    let f = p.session_file(prefix, session, ".json")?;
    serde_json::from_slice(&std::fs::read(f).ok()?).ok()
}

/// Сдвиг `at` в `snapst-<сессия>.json` на `ms` назад (время «60 с назад»).
fn age_snapst(p: &Proj, session: &str, ms: u64) {
    let f = p.session_file("snapst", session, ".json").expect("snapst");
    let mut v: Value = serde_json::from_slice(&std::fs::read(&f).unwrap()).unwrap();
    v["at"] = json!(v["at"].as_u64().unwrap() - ms);
    std::fs::write(&f, v.to_string()).unwrap();
}

fn prompts_of(body: &Value) -> Vec<String> {
    body["data"]["prompts"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// §8а: жёсткий срок снимка — `decision: block` у `Stop` не ждёт сборки дольше
/// `hooks.snapshot_ms` (поток сборки «засыпает» на 3 с через точку подмены `<журнал>.stall-ms`);
/// опоздавший снимок не кладётся ни в очередь, ни на сервер.
#[test]
fn snapshot_deadline_never_delays_stop() {
    let _g = serial();
    let (url, seen) = snap_stub(204, "");
    let p = Proj::new("snap-deadline", &url, "zam_x");
    let t = transcript_path(&p, "sd-1");
    write_lines(&t, &[cl_user("sd-1", "Запрос перед остановкой")]);
    std::fs::write(format!("{}.stall-ms", t.display()), "3000").unwrap();
    // правки в сессии — напоминание `Stop` (GET /sessions → записей 0)
    empty(&p.hook("pre-tool", &write_ev(&p, "sd-1", "a.sh")));
    let warm = snap_ev(
        &p,
        "Stop",
        "sd-warm",
        &t,
        json!({"stop_hook_active": false}),
    );
    let _ = p.hook_at(&url, "checkpoint", "claude", warm.to_string().as_bytes());
    let ev = snap_ev(&p, "Stop", "sd-1", &t, json!({"stop_hook_active": false}));
    let (o, d) = p.hook_at(&url, "checkpoint", "claude", ev.to_string().as_bytes());
    let v = hook_json(&o);
    assert_eq!(v["decision"], "block", "{v}");
    assert!(d <= max_for("checkpoint"), "снимок задержал Stop: {d:?}");
    assert!(puts(&seen).is_empty(), "{:?}", puts(&seen));
    assert!(snap_file(&p, "snapq", "sd-1").is_none());
}

/// §8а: читается только хвост `hooks.snapshot_tail_kb` растущего журнала — старый запрос за
/// окном в снимок не попадает, новый (дописанный после прошлого снимка) — попадает.
#[test]
fn snapshot_tail_of_growing_file() {
    let _g = serial();
    let (url, seen) = snap_stub(204, "");
    let p = Proj::new("snap-tail", &url, "zam_x");
    project_hooks(&p, "snapshot_tail_kb = 64");
    let t = transcript_path(&p, "st-1");
    let s = "st-1";
    let mut lines = vec![cl_user(s, "старый-запрос за окном")];
    lines.extend((0..40).map(|_| cl_filler(4096)));
    lines.push(cl_user(s, "новый-1"));
    write_lines(&t, &lines);
    let stop = snap_ev(&p, "Stop", s, &t, json!({"stop_hook_active": false}));
    empty(&hook_snap(&p, "checkpoint", &stop));
    let got = puts(&seen);
    assert_eq!(got.len(), 1, "{got:?}: {}", p.log());
    assert_eq!(got[0].0, format!("/api/v1/sessions/{s}/snapshot"));
    assert_eq!(prompts_of(&got[0].1), ["новый-1"], "{}", got[0].1);
    assert_eq!(got[0].1["trigger"], "stop");
    // журнал растёт: дописанный запрос — в следующем снимке
    let mut more: Vec<String> = (0..5).map(|_| cl_filler(4096)).collect();
    more.push(cl_user(s, "новый-2"));
    append_lines(&t, &more);
    age_snapst(&p, s, 61_000);
    empty(&hook_snap(&p, "checkpoint", &stop));
    let got = puts(&seen);
    assert_eq!(got.len(), 2, "{}", p.log());
    assert_eq!(
        prompts_of(&got[1].1),
        ["новый-1", "новый-2"],
        "{}",
        got[1].1
    );
    let st = snap_file(&p, "snapst", s).unwrap();
    assert_eq!(st["size"], std::fs::metadata(&t).unwrap().len());
}

/// §8а: журнала нет, путь — каталог или пустая строка — снимка нет, вывода нет, ошибок в
/// `hooks.log` нет, `snapst-*`/`snapq-*` не появляются.
#[test]
fn snapshot_bad_path_is_silent() {
    let _g = serial();
    let (url, seen) = snap_stub(204, "");
    let p = Proj::new("snap-bad-path", &url, "zam_x");
    let dir = p.home.join("not-a-file");
    std::fs::create_dir_all(&dir).unwrap();
    for (i, t) in [
        p.home.join("нет-такого.jsonl"),
        dir.clone(),
        PathBuf::from(""),
    ]
    .iter()
    .enumerate()
    {
        let s = format!("bp-{i}");
        for (event, extra) in [
            ("Stop", json!({"stop_hook_active": false})),
            ("PreCompact", json!({"trigger": "auto"})),
            ("SessionEnd", json!({"reason": "exit"})),
        ] {
            empty(&hook_snap(
                &p,
                "checkpoint",
                &snap_ev(&p, event, &s, t, extra),
            ));
        }
        assert!(snap_file(&p, "snapst", &s).is_none(), "{t:?}");
        assert!(snap_file(&p, "snapq", &s).is_none(), "{t:?}");
    }
    // файл без единой записи формата (мусор, вывод агента) — снимок пустой: ни `PUT`, ни
    // очереди, прежний снимок на сервере не затирается
    let junk = p.home.join("junk.jsonl");
    let mut bytes = vec![0xffu8, 0xfe, b'\n'];
    bytes.extend(cl_filler(2048).into_bytes());
    bytes.push(b'\n');
    std::fs::write(&junk, bytes).unwrap();
    let pc = snap_ev(
        &p,
        "PreCompact",
        "bp-junk",
        &junk,
        json!({"trigger": "auto"}),
    );
    empty(&hook_snap(&p, "checkpoint", &pc));
    assert!(snap_file(&p, "snapq", "bp-junk").is_none());
    assert!(puts(&seen).is_empty());
    let log = p.log();
    assert!(
        !log.contains("снимок") && !log.contains("snapshot"),
        "{log}"
    );
}

/// §8а: `Stop` снимает при росте журнала ≥ 16 КиБ или новом запросе человека (`Ev::Prompt`
/// журнала сессии) и не чаще раза в 60 с («60 с назад» — правка `at` в `snapst-*`).
#[test]
fn stop_throttle_16kib_60s() {
    let _g = serial();
    let (url, seen) = snap_stub(204, "");
    let p = Proj::new("snap-throttle", &url, "zam_x");
    let s = "th-1";
    let t = transcript_path(&p, s);
    write_lines(&t, &[cl_user(s, "запрос A")]);
    let stop = snap_ev(&p, "Stop", s, &t, json!({"stop_hook_active": false}));
    empty(&hook_snap(&p, "checkpoint", &stop));
    assert_eq!(puts(&seen).len(), 1, "первый Stop: {}", p.log());
    // мало времени: новый запрос не снимается
    append_lines(&t, &[cl_user(s, "запрос B")]);
    empty(&hook_snap(&p, "checkpoint", &stop));
    assert_eq!(puts(&seen).len(), 1, "чаще раза в 60 с");
    // 60 с прошло, но журнал вырос меньше 16 КиБ и запроса через хук не было
    age_snapst(&p, s, 61_000);
    empty(&hook_snap(&p, "checkpoint", &stop));
    assert_eq!(puts(&seen).len(), 1, "рост < 16 КиБ без запроса");
    // запрос человека (хук `prompt` пишет `Ev::Prompt`) — снимок
    let pr = snap_ev(&p, "UserPromptSubmit", s, &t, json!({"prompt": "запрос B"}));
    ok(&hook_snap(&p, "prompt", &pr));
    empty(&hook_snap(&p, "checkpoint", &stop));
    let got = puts(&seen);
    assert_eq!(got.len(), 2, "новый запрос: {}", p.log());
    assert_eq!(prompts_of(&got[1].1), ["запрос A", "запрос B"]);
    // рост ≥ 16 КиБ без запроса — снимок (после 60 с)
    age_snapst(&p, s, 61_000);
    append_lines(&t, &(0..5).map(|_| cl_filler(4096)).collect::<Vec<_>>());
    empty(&hook_snap(&p, "checkpoint", &stop));
    assert_eq!(puts(&seen).len(), 3, "рост ≥ 16 КиБ: {}", p.log());
    // и снова рано
    append_lines(&t, &(0..5).map(|_| cl_filler(4096)).collect::<Vec<_>>());
    empty(&hook_snap(&p, "checkpoint", &stop));
    assert_eq!(puts(&seen).len(), 3);
}

/// §8а: `PostCompact` и `SessionEnd` — события `checkpoint` без вывода в бюджете хука;
/// `PostCompact` — конспект из входа хука плюс запросы и команды журнала, `SessionEnd` — снимок
/// и `PATCH /sessions/{id} {ended: true}`.
#[test]
fn post_compact_and_session_end_accepted_within_100ms() {
    let _g = serial();
    let (url, seen) = snap_stub(204, "");
    let p = Proj::new("snap-pc-end", &url, "zam_x");
    let s = "pc-1";
    let t = transcript_path(&p, s);
    write_lines(
        &t,
        &[
            cl_user(s, "запрос до сжатия"),
            cl_bash(s, "b1", "cargo test -p demo"),
        ],
    );
    let warm = snap_ev(
        &p,
        "Stop",
        "pc-warm",
        &t,
        json!({"stop_hook_active": false}),
    );
    let _ = p.hook_at(&url, "checkpoint", "claude", warm.to_string().as_bytes());
    let before = puts(&seen).len();
    let pc = snap_ev(
        &p,
        "PostCompact",
        s,
        &t,
        json!({"trigger": "auto", "compact_summary": "8. Current Work:\nмодуль A\n9. Optional Next Step:\nтесты модуля A"}),
    );
    let (o, d) = p.hook_at(&url, "checkpoint", "claude", pc.to_string().as_bytes());
    empty(&o);
    assert!(d <= max_for("checkpoint"), "PostCompact: {d:?}");
    let end = snap_ev(&p, "SessionEnd", s, &t, json!({"reason": "exit"}));
    let (o, d) = p.hook_at(&url, "checkpoint", "claude", end.to_string().as_bytes());
    empty(&o);
    assert!(d <= max_for("checkpoint"), "SessionEnd: {d:?}");
    let got: Vec<(String, Value)> = puts(&seen).into_iter().skip(before).collect();
    // срок по умолчанию (25 мс) на отладочной сборке под нагрузкой бывает мал — тогда снимок
    // пропускается (это и есть жёсткий срок); проверяется то, что дошло
    for (path, b) in &got {
        assert_eq!(path, &format!("/api/v1/sessions/{s}/snapshot"));
        match b["trigger"].as_str() {
            Some("post_compact") => {
                let sum = b["data"]["compact_summary"].as_str().unwrap();
                assert!(sum.contains("тесты модуля A"), "{b}");
                assert_eq!(prompts_of(b), ["запрос до сжатия"], "{b}");
                assert_eq!(b["data"]["commands"][0]["cmd"], "cargo test -p demo");
            }
            Some("session_end") => assert!(b["data"].get("compact_summary").is_none(), "{b}"),
            other => panic!("повод {other:?}: {b}"),
        }
    }
    // содержимое — со щедрым сроком: оба снимка дошли
    let before = puts(&seen).len();
    empty(&hook_snap(&p, "checkpoint", &pc));
    empty(&hook_snap(&p, "checkpoint", &end));
    let triggers: Vec<String> = puts(&seen)
        .into_iter()
        .skip(before)
        .map(|(_, b)| b["trigger"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(triggers, ["post_compact", "session_end"], "{}", p.log());
    assert_eq!(snap_file(&p, "snapst", s).unwrap()["ended"], true);
    let patched: Vec<Value> = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|(m, path, _)| m == "PATCH" && path == &format!("/api/v1/sessions/{s}"))
        .map(|(_, _, b)| b.clone())
        .collect();
    assert!(!patched.is_empty(), "{}", p.log());
    assert!(
        patched.iter().all(|b| b == &json!({"ended": true})),
        "{patched:?}"
    );
}

/// §8а: `SessionEnd` ставит сессии `ended_at` настоящего сервера — и при выключенном снимке
/// (`hooks.session_snapshot = false`); сервер без `PUT …/snapshot` (404) — снимок в очереди.
#[test]
fn session_end_patches_ended() {
    let _g = serial();
    let (url, tok) = start("snap-ended");
    let p = Proj::new("snap-ended", &url, &tok);
    let reg = |id: &str| {
        let r = http()
            .post(&format!("{url}/api/v1/sessions"))
            .header("Authorization", format!("Bearer {tok}"))
            .content_type("application/json")
            .send(format!(r#"{{"id":"{id}","agent":"claude-code"}}"#))
            .unwrap();
        assert_eq!(r.status(), 201);
    };
    for (s, on) in [("se-on", true), ("se-off", false)] {
        reg(s);
        project_hooks(&p, &format!("session_snapshot = {on}"));
        assert!(p.session(s)["ended_at"].is_null());
        let t = transcript_path(&p, s);
        write_lines(&t, &[cl_user(s, "последний запрос")]);
        let end = snap_ev(&p, "SessionEnd", s, &t, json!({"reason": "exit"}));
        empty(&hook_snap(&p, "checkpoint", &end));
        assert!(
            p.session(s)["ended_at"].is_string(),
            "{s}: {} / {}",
            p.session(s),
            p.log()
        );
        assert_eq!(snap_file(&p, "snapq", s).is_some(), on, "{s}: {}", p.log());
    }
    // сессии нет на сервере — молча
    let t = transcript_path(&p, "se-none");
    let end = snap_ev(&p, "SessionEnd", "se-none", &t, json!({"reason": "exit"}));
    empty(&hook_snap(&p, "checkpoint", &end));
    assert!(!p.log().contains("sessions(ended)"), "{}", p.log());
}

/// §8а: `SessionStart source=compact` без `PostCompact` — снимок из журнала, `PUT` **до**
/// `/recall`; `PostCompact` пришёл (свежий `snapst-*` с поводом `post_compact`) — журнал не
/// читается, нового снимка нет, неотправленная очередь сессии уходит до `/recall`.
#[test]
fn compact_put_before_recall() {
    let _g = serial();
    let (url, seen) = snap_stub(204, "");
    let p = Proj::new("snap-compact", &url, "zam_x");
    let s = "cp-1";
    let t = transcript_path(&p, s);
    write_lines(&t, &[cl_user(s, "запрос перед сжатием")]);
    let ss = snap_ev(&p, "SessionStart", s, &t, json!({"source": "compact"}));
    let c = context(&hook_snap(&p, "session-start", &ss), "SessionStart");
    assert!(c.starts_with('D'), "{c}");
    let put = req_index(&seen, 0, "PUT", |x| {
        x == format!("/api/v1/sessions/{s}/snapshot")
    });
    let recall = req_index(&seen, 0, "POST", |x| x == "/api/v1/recall");
    assert!(
        put.is_some() && put < recall,
        "PUT {put:?}, recall {recall:?}"
    );
    assert_eq!(puts(&seen)[0].1["trigger"], "session_start_compact");

    // PostCompact уже снял (сервер не ответил — снимок в очереди), затем старт после сжатия
    let (url2, _seen2) = snap_stub(503, r#"{"status":503,"detail":"занят"}"#);
    let q = Proj::new("snap-compact-pc", &url2, "zam_x");
    let t2 = transcript_path(&q, s);
    write_lines(&t2, &[cl_user(s, "запрос перед сжатием")]);
    let pc = snap_ev(
        &q,
        "PostCompact",
        s,
        &t2,
        json!({"trigger": "auto", "compact_summary": "Конспект"}),
    );
    empty(&hook_snap(&q, "checkpoint", &pc));
    assert!(snap_file(&q, "snapq", s).is_some(), "{}", q.log());
    // и очередь другой сессии (её `SessionEnd` не дошёл) — уходит со стартом, параллельно
    let o2 = "cp-other";
    let t3 = transcript_path(&q, o2);
    write_lines(&t3, &[cl_user(o2, "запрос другой сессии")]);
    let end = snap_ev(&q, "SessionEnd", o2, &t3, json!({"reason": "exit"}));
    empty(&hook_snap(&q, "checkpoint", &end));
    assert!(snap_file(&q, "snapq", o2).is_some(), "{}", q.log());
    // сервер ожил: тот же адрес нельзя — новая заглушка, адрес — через ZAM_URL
    let (url3, seen3) = snap_stub(204, "");
    let ss = snap_ev(&q, "SessionStart", s, &t2, json!({"source": "compact"}));
    let o = q
        .cmd(&url3)
        .env("ZAM_TEST_HOOK_BUDGET_MS", "10000")
        .env("ZAM_TEST_SNAPSHOT_MS", "5000")
        .args(["hook", "session-start", "--agent", "claude"])
        .write_stdin(ss.to_string().into_bytes())
        .output()
        .unwrap();
    context(&o, "SessionStart");
    let got = puts(&seen3);
    assert_eq!(got.len(), 2, "{got:?}: {}", q.log());
    let own: Vec<&Value> = got
        .iter()
        .filter(|(x, _)| x == &format!("/api/v1/sessions/{s}/snapshot"))
        .map(|(_, b)| b)
        .collect();
    assert_eq!(own.len(), 1, "{got:?}");
    assert_eq!(own[0]["trigger"], "post_compact", "журнал прочитан заново");
    assert!(
        got.iter()
            .any(|(x, b)| x == &format!("/api/v1/sessions/{o2}/snapshot")
                && b["trigger"] == "session_end"),
        "{got:?}"
    );
    assert!(snap_file(&q, "snapq", o2).is_none());
    let put = req_index(&seen3, 0, "PUT", |x| {
        x == format!("/api/v1/sessions/{s}/snapshot")
    });
    let recall = req_index(&seen3, 0, "POST", |x| x == "/api/v1/recall");
    assert!(
        put.is_some() && put < recall,
        "PUT {put:?}, recall {recall:?}"
    );
    assert!(snap_file(&q, "snapq", s).is_none());
}

/// §8а: догоняющий снимок на `session-start source=startup` — у последней сессии проекта нет
/// `SessionEnd`, а её журнал вырос после снимка: снимок той сессии (повод `catch_up`) уходит
/// **до** `/recall`; сессия закончилась или журнал не вырос — догоняющего снимка нет.
#[test]
fn catch_up_before_recall_on_startup() {
    let _g = serial();
    let (url, seen) = snap_stub(204, "");
    let p = Proj::new("snap-catch-up", &url, "zam_x");
    let a = "cu-a";
    let t = transcript_path(&p, a);
    write_lines(&t, &[cl_user(a, "запрос 1")]);
    let stop = snap_ev(&p, "Stop", a, &t, json!({"stop_hook_active": false}));
    empty(&hook_snap(&p, "checkpoint", &stop));
    assert_eq!(puts(&seen).len(), 1, "{}", p.log());
    // сессия оборвалась после ещё одного запроса и правки
    append_lines(
        &t,
        &[
            cl_user(a, "запрос 2 перед обрывом"),
            cl_edit(a, "e1", &p.repo.join("src/a.rs")),
        ],
    );
    let start_of = |s: &str| {
        let tb = transcript_path(&p, s);
        write_lines(&tb, &[]);
        snap_ev(&p, "SessionStart", s, &tb, json!({"source": "startup"}))
    };
    let n0 = seen.lock().unwrap().len();
    context(
        &hook_snap(&p, "session-start", &start_of("cu-b")),
        "SessionStart",
    );
    let got = puts(&seen);
    assert_eq!(got.len(), 2, "{}", p.log());
    assert_eq!(got[1].0, format!("/api/v1/sessions/{a}/snapshot"));
    assert_eq!(got[1].1["trigger"], "catch_up");
    assert_eq!(
        prompts_of(&got[1].1),
        ["запрос 1", "запрос 2 перед обрывом"]
    );
    assert_eq!(got[1].1["data"]["files"], json!(["src/a.rs"]));
    let put = req_index(&seen, n0, "PUT", |x| x.contains(a));
    let recall = req_index(&seen, n0, "POST", |x| x == "/api/v1/recall");
    assert!(
        put.is_some() && put < recall,
        "PUT {put:?}, recall {recall:?}"
    );
    // журнал не вырос — второго догоняющего снимка нет
    context(
        &hook_snap(&p, "session-start", &start_of("cu-c")),
        "SessionStart",
    );
    assert_eq!(puts(&seen).len(), 2, "{}", p.log());
    // сессия закончилась (`SessionEnd`) — и после роста журнала догоняющего снимка нет
    let end = snap_ev(&p, "SessionEnd", a, &t, json!({"reason": "exit"}));
    empty(&hook_snap(&p, "checkpoint", &end));
    let n = puts(&seen).len();
    append_lines(&t, &[cl_user(a, "запись после конца")]);
    context(
        &hook_snap(&p, "session-start", &start_of("cu-d")),
        "SessionStart",
    );
    assert_eq!(puts(&seen).len(), n, "{}", p.log());
}

/// §8а: старый сервер (404 на `PUT`) — не больше 3 попыток за сессию, затем снимок остаётся
/// в `snapq-*`; хук молчит, код 0; новые снимки заменяют старый в очереди.
#[test]
fn old_server_three_attempts_then_queue() {
    let _g = serial();
    let (url, seen) = snap_stub(
        404,
        r#"{"type":"about:blank","status":404,"title":"Not Found"}"#,
    );
    let p = Proj::new("snap-old-server", &url, "zam_x");
    let s = "os-1";
    let t = transcript_path(&p, s);
    write_lines(&t, &[cl_user(s, "запрос 0")]);
    for i in 0..5 {
        append_lines(&t, &[cl_user(s, &format!("запрос {}", i + 1))]);
        let pc = snap_ev(&p, "PreCompact", s, &t, json!({"trigger": "auto"}));
        empty(&hook_snap(&p, "checkpoint", &pc));
    }
    assert_eq!(puts(&seen).len(), 3, "{}", p.log());
    let q = snap_file(&p, "snapq", s).expect("снимок в очереди");
    assert_eq!(q["old_server"], 3, "{q}");
    assert_eq!(q["session"], s);
    assert_eq!(prompts_of(&q["body"]).last().unwrap(), "запрос 5", "{q}");
    let log = p.log();
    assert!(log.contains("попытка 3 из 3"), "{log}");
    assert_eq!(log.matches("попытка").count(), 3, "{log}");
}

/// §8а: `hooks.session_snapshot = false` — журнал агента не открывается: нет `snapst-*`,
/// `snapq-*`, `PUT` и `Ev::Snapshot`, догоняющего снимка тоже нет (точка подмены
/// `<журнал>.stall-ms` задержала бы хук на 2 с при сроке 5 с); `SessionEnd` ставит `ended`.
#[test]
fn switch_off_never_opens_transcript() {
    let _g = serial();
    let (url, seen) = snap_stub(204, "");
    let p = Proj::new("snap-off", &url, "zam_x");
    let s = "off-1";
    let t = transcript_path(&p, s);
    write_lines(&t, &[cl_user(s, "запрос")]);
    // контроль: при включённом снимке подмена задерживает хук
    std::fs::write(format!("{}.stall-ms", t.display()), "2000").unwrap();
    let pc = snap_ev(&p, "PreCompact", "off-on", &t, json!({"trigger": "auto"}));
    let t0 = Instant::now();
    empty(&hook_snap(&p, "checkpoint", &pc));
    assert!(
        t0.elapsed() >= Duration::from_millis(1900),
        "подмена не сработала"
    );
    // её журнал вырос — при включённом снимке старт сделал бы догоняющий снимок
    append_lines(&t, &[cl_user("off-on", "ещё запрос")]);
    project_hooks(&p, "session_snapshot = false");
    let n = puts(&seen).len();
    let quick = |o: Output, t0: Instant, what: &str| {
        assert!(
            t0.elapsed() < Duration::from_millis(1900),
            "{what}: журнал открывался ({:?})",
            t0.elapsed()
        );
        o
    };
    for (event, extra) in [
        ("Stop", json!({"stop_hook_active": false})),
        ("PreCompact", json!({"trigger": "auto"})),
        (
            "PostCompact",
            json!({"trigger": "auto", "compact_summary": "К"}),
        ),
        ("SessionEnd", json!({"reason": "exit"})),
    ] {
        let t0 = Instant::now();
        let o = hook_snap(&p, "checkpoint", &snap_ev(&p, event, s, &t, extra));
        empty(&quick(o, t0, event));
    }
    for src in ["compact", "startup"] {
        let ss = snap_ev(&p, "SessionStart", "off-2", &t, json!({"source": src}));
        let t0 = Instant::now();
        let o = hook_snap(&p, "session-start", &ss);
        context(&quick(o, t0, src), "SessionStart");
    }
    assert_eq!(puts(&seen).len(), n, "{}", p.log());
    for f in ["snapst", "snapq"] {
        assert!(snap_file(&p, f, s).is_none(), "{f}");
        assert!(snap_file(&p, f, "off-2").is_none(), "{f}");
    }
    assert!(!events(&p, s).iter().any(|e| e["ev"] == "snapshot"));
    let patched = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|(m, path, _)| m == "PATCH" && path == &format!("/api/v1/sessions/{s}"))
        .count();
    assert_eq!(patched, 1);
}

/// §8а: очередь снимка (в ней текст) и состояние — с правами 0600.
#[cfg(unix)]
#[test]
fn queue_file_mode_0600() {
    use std::os::unix::fs::PermissionsExt;
    let _g = serial();
    let (url, _seen) = snap_stub(404, r#"{"status":404}"#);
    let p = Proj::new("snap-mode", &url, "zam_x");
    let s = "md-1";
    let t = transcript_path(&p, s);
    write_lines(&t, &[cl_user(s, "запрос")]);
    let pc = snap_ev(&p, "PreCompact", s, &t, json!({"trigger": "auto"}));
    empty(&hook_snap(&p, "checkpoint", &pc));
    for prefix in ["snapq", "snapst"] {
        let f = p.session_file(prefix, s, ".json").expect(prefix);
        let mode = std::fs::metadata(&f).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{prefix}: {mode:o}");
    }
}

/// Приманки снимка: запрос с маркером и ключом, команда с паролем во флаге.
fn snap_baits() -> (String, String, String) {
    let gh = [concat!("gh", "p_"), "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8"].concat();
    let pw = ["Zq7", "xW2", "pL9"].concat();
    (gh, pw, "ПРИМАНКА-ЗАПРОСА".to_string())
}

/// §8а: в `hooks.log` клиента — только событие, размер и статус снимка: ни текста запросов,
/// ни команд, ни конспекта, ни секретов; в очередь секреты уходят уже замаскированными.
#[test]
fn hooks_log_has_no_snapshot_text() {
    let _g = serial();
    let (gh, pw, marker) = snap_baits();
    for (put, body) in [(404u16, r#"{"status":404}"#), (204, "")] {
        let (url, seen) = snap_stub(put, body);
        let p = Proj::new(&format!("snap-log-{put}"), &url, "zam_x");
        let s = "lg-1";
        let t = transcript_path(&p, s);
        write_lines(
            &t,
            &[
                cl_user(s, &format!("{marker}: ключ {gh}")),
                cl_bash(
                    s,
                    "b1",
                    &format!("mysql -uroot --password {pw} -e 'КОМАНДА-ПРИМАНКА'"),
                ),
            ],
        );
        let pc = snap_ev(
            &p,
            "PostCompact",
            s,
            &t,
            json!({"trigger": "auto", "compact_summary": "КОНСПЕКТ-ПРИМАНКА"}),
        );
        empty(&hook_snap(&p, "checkpoint", &pc));
        let log = p.log();
        assert!(log.contains("снимок post_compact: "), "{log}");
        for bad in [
            marker.as_str(),
            &gh,
            &pw,
            "КОМАНДА-ПРИМАНКА",
            "КОНСПЕКТ-ПРИМАНКА",
            "mysql",
        ] {
            assert!(!log.contains(bad), "{put}: {bad} в hooks.log: {log}");
        }
        // до сервера и в очередь — уже замаскированное
        let sent: Vec<Value> = puts(&seen).into_iter().map(|(_, b)| b).collect();
        let q = snap_file(&p, "snapq", s).map(|q| q["body"].clone());
        let bodies: Vec<Value> = sent.into_iter().chain(q).collect();
        assert!(!bodies.is_empty(), "{log}");
        for b in bodies {
            let text = b.to_string();
            assert!(text.contains(&marker), "{text}");
            assert!(!text.contains(&gh) && !text.contains(&pw), "секрет: {text}");
            assert!(text.contains("‹secret›"), "{text}");
        }
    }
}

/// §8а: событие `Ev::Snapshot` журнала сессии — только счётчики (повод, полнота, байты,
/// запросов, файлов, команд), без текста; сводка `recovery` получает строку «снимок сессии
/// <yyyy-MM-dd HH:mm:ss>, извлечено автоматически».
#[test]
fn ev_snapshot_metadata_only() {
    let _g = serial();
    let (_, _, marker) = snap_baits();
    let (url, _seen) = snap_stub(204, "");
    let p = Proj::new("snap-ev", &url, "zam_x");
    let s = "ev-1";
    let t = transcript_path(&p, s);
    write_lines(
        &t,
        &[
            cl_user(s, &marker),
            cl_edit(s, "e1", &p.repo.join("src/lib.rs")),
            cl_bash(s, "b1", "cargo build --release"),
        ],
    );
    let pc = snap_ev(&p, "PreCompact", s, &t, json!({"trigger": "auto"}));
    empty(&hook_snap(&p, "checkpoint", &pc));
    let evs: Vec<Value> = events(&p, s)
        .into_iter()
        .filter(|e| e["ev"] == "snapshot")
        .collect();
    assert_eq!(evs.len(), 1, "{}", p.log());
    let e = &evs[0];
    let mut keys: Vec<&str> = e.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "bytes", "cmds", "ev", "files", "prompts", "status", "trigger", "ts"
        ],
        "{e}"
    );
    assert_eq!(
        (
            &e["trigger"],
            &e["status"],
            &e["prompts"],
            &e["files"],
            &e["cmds"]
        ),
        (
            &json!("pre_compact"),
            &json!("complete"),
            &json!(1),
            &json!(1),
            &json!(1)
        )
    );
    assert!(e["bytes"].as_u64().unwrap() > 0);
    let raw = std::fs::read_to_string(p.session_file("events", s, ".jsonl").unwrap()).unwrap();
    for bad in [marker.as_str(), "cargo build", "src/lib.rs"] {
        assert!(!raw.contains(bad), "{bad}: {raw}");
    }
    // сводка восстановления после compact
    let ss = snap_ev(&p, "SessionStart", s, &t, json!({"source": "compact"}));
    let c = context(&hook_snap(&p, "session-start", &ss), "SessionStart");
    let re = regex::Regex::new(
        r"снимок сессии \d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}, извлечено автоматически",
    )
    .unwrap();
    assert!(re.is_match(&c), "{c}");
}
