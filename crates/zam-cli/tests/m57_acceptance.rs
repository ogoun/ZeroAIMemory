//! Приёмка M5.7 «Идеи» (спецификация `docs/specs/2026-11-04-m5.7-ideas.md` §13, задача 6),
//! клиентская часть против настоящего zam-server в процессе теста и настоящего `zam`:
//!
//! - `acceptance_cli_idea_flow` (п. 13) — `zam idea new|list|propose|accept|defer|decline`
//!   делают то же, что REST; под агентом без `--by`/`--origin` — код 6, повторный `accept` —
//!   код 4 с задачей, 403 — код 3;
//! - `acceptance_hook_session_start_shows_idea` (п. 1 через хук) — `zam hook session-start`
//!   (Claude Code и `--agent codex`) выводит раздел `### Идеи` со ссылкой `w:`;
//! - `acceptance_recall_p95_with_50_ideas` (п. 15) — p95 `session-start` и `prompt` решения с
//!   50 идеями против решения без идей (вперемешку, тот же сервер): прирост ≤ 5 и ≤ 15 мс.
//!
//! Серверная часть — `crates/zam-server/tests/m57_acceptance.rs`. Сервер без моделей (FTS),
//! каталоги — в `CARGO_TARGET_TMPDIR`.
mod common;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Output;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use zam_server::build;
use zam_server::config::ServerConfig;
use zam_store::Access;

/// Решение с идеями.
const GA: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9c57";
/// Решение без идей (база замера p95).
const GB: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9c58";

fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
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

/// Сервер в фоновом потоке: адрес и токен `rw` пользователя «Анна» с решениями `GA`, `GB`.
fn start(name: &str) -> (String, String) {
    let data = common::sandbox("cli-m57", &format!("{name}-data"));
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
    for (g, name) in [(GA, "Идеи"), (GB, "Без идей")] {
        let (code, v) = rest(
            &url,
            &tok,
            "POST",
            "/solutions",
            Some(&json!({"agent_solution_id": g, "name": name})),
        );
        assert_eq!(code, 201, "{v}");
    }
    (url, tok)
}

fn rest(url: &str, tok: &str, method: &str, path: &str, body: Option<&Value>) -> (u16, Value) {
    let a = http();
    let uri = format!("{url}/api/v1{path}");
    let auth = format!("Bearer {tok}");
    let r = match method {
        "GET" => a.get(&uri).header("Authorization", &auth).call(),
        _ => a
            .post(&uri)
            .header("Authorization", &auth)
            .content_type("application/json")
            .send(body.map(Value::to_string).unwrap_or_default()),
    };
    let mut r = r.unwrap_or_else(|e| panic!("{method} {path}: {e}"));
    let code = r.status().as_u16();
    let text = r.body_mut().read_to_string().unwrap_or_default();
    let v = if text.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path}: не JSON ({e}): {text}"))
    };
    (code, v)
}

/// Проект решения `g`: каталог с `solution.toml` и «домашний» каталог.
struct Proj {
    url: String,
    token: String,
    repo: PathBuf,
    home: PathBuf,
}

impl Proj {
    fn new(name: &str, url: &str, token: &str, g: &str) -> Proj {
        let repo = common::sandbox("cli-m57", name);
        let home = repo.parent().unwrap().join(format!("{name}-home"));
        std::fs::create_dir_all(home.join(".zeroaimemory")).unwrap();
        std::fs::write(
            home.join(".zeroaimemory/config.toml"),
            "[code]\nauto = false\n",
        )
        .unwrap();
        std::fs::create_dir_all(repo.join(".zeroaimemory")).unwrap();
        std::fs::write(
            repo.join(".zeroaimemory/solution.toml"),
            format!("agent_solution_id = \"{g}\"\nname = \"M5.7\"\n"),
        )
        .unwrap();
        Proj {
            url: url.into(),
            token: token.into(),
            repo,
            home,
        }
    }

    fn cmd(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut c = common::zam_cmd();
        c.env("ZAM_URL", &self.url)
            .env("ZAM_TOKEN", &self.token)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .current_dir(&self.repo)
            .args(args)
            .write_stdin(Vec::new());
        for (k, v) in env {
            c.env(k, v);
        }
        c.output().unwrap()
    }

    /// Человек в терминале.
    fn z(&self, args: &[&str]) -> Output {
        self.cmd(args, &[])
    }

    /// Команду запустил агент (`ZAM_AGENT=1`).
    fn agent(&self, args: &[&str]) -> Output {
        self.cmd(args, &[("ZAM_AGENT", "1"), ("ZAM_SESSION", "-")])
    }

    fn get(&self, id: &str) -> Value {
        let (code, v) = rest(
            &self.url,
            &self.token,
            "GET",
            &format!("/entries/{id}"),
            None,
        );
        assert_eq!(code, 200, "{v}");
        v
    }

    /// `zam hook <kind> --agent <agent>` с событием `ev`: вывод и время процесса.
    fn hook(&self, kind: &str, agent: &str, ev: &Value) -> (Output, Duration) {
        let mut c = common::zam_cmd();
        c.env("ZAM_URL", &self.url)
            .env("ZAM_TOKEN", &self.token)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .current_dir(&self.repo)
            .args(["hook", kind, "--agent", agent])
            .write_stdin(ev.to_string().into_bytes());
        let t = Instant::now();
        let o = c.output().unwrap();
        (o, t.elapsed())
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
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn ok(o: &Output) -> String {
    assert_eq!(
        o.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        out(o),
        err(o)
    );
    out(o)
}

fn code(o: &Output) -> Option<i32> {
    o.status.code()
}

/// Голый id из `<id>@<rev>` вывода `zam idea new`.
fn new_id(o: &Output) -> String {
    ok(o).trim().split_once('@').unwrap().0.to_string()
}

/// `additionalContext` вывода хука; пусто — пустая строка.
fn context(o: &Output) -> String {
    let s = ok(o);
    if s.trim().is_empty() {
        return String::new();
    }
    let v: Value = serde_json::from_str(s.trim()).unwrap_or_else(|e| panic!("{e}: {s}"));
    v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn p95(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[(v.len() * 95).div_ceil(100) - 1]
}

// ---------- п. 13 ----------

/// §13 п. 13: `zam idea` против REST — создание (`origin` человека по умолчанию, под агентом —
/// обязателен, код 6), лента, предложение (вопрос `q:` с тремя вариантами), «начать» (задача
/// `k:`, повтор — код 4 с задачей), отсрочка до даты, отказ; `accept --by agent` по идее
/// человека — 403, код 3; под агентом без `--by` — код 6.
#[test]
fn acceptance_cli_idea_flow() {
    let _g = serial();
    let (url, tok) = start("flow");
    let p = Proj::new("flow", &url, &tok, GA);
    // создание: человек — origin human; агент без --origin — код 6, с --origin — как сказано
    let id = new_id(&p.z(&["idea", "new", "Экспорт в PDF", "--priority", "high"]));
    let e = p.get(&id);
    assert_eq!(
        (e["kind"].as_str(), e["status"].as_str()),
        (Some("idea"), Some("new"))
    );
    assert_eq!(e["data"]["origin"], "human");
    assert_eq!(e["data"]["priority"], "high");
    let o = p.agent(&["idea", "new", "Без origin"]);
    assert_eq!(code(&o), Some(6), "{}", err(&o));
    let mine = new_id(&p.agent(&["idea", "new", "Своя мысль", "--origin", "agent"]));
    assert_eq!(p.get(&mine)["data"]["origin"], "agent");
    // лента — та же, что REST
    let l = ok(&p.z(&["idea", "list"]));
    assert!(
        l.contains(&format!("w:{id} [new] [high] Экспорт в PDF")),
        "{l}"
    );
    assert!(
        l.contains(&format!("w:{mine} [new] [normal] Своя мысль (от агента)")),
        "{l}"
    );
    let (c, feed) = rest(
        &url,
        &tok,
        "GET",
        &format!("/spaces/{GA}/entries?kind=idea"),
        None,
    );
    assert_eq!(c, 200);
    assert_eq!(feed["items"].as_array().unwrap().len(), 2, "{feed}");
    // предложение → вопрос с тремя вариантами
    let o = ok(&p.agent(&["idea", "propose", &format!("w:{id}"), "--text", "пора"]));
    let qid = o
        .trim()
        .rsplit_once("q:")
        .map(|(_, q)| q.trim().to_string())
        .unwrap_or_else(|| panic!("{o}"));
    assert_eq!(p.get(&id)["status"], "proposed");
    let q = p.get(&qid);
    assert_eq!(q["status"], "open", "{q}");
    assert_eq!(q["data"]["options"].as_array().unwrap().len(), 3, "{q}");
    // «начать» под агентом без --by — код 6; с --by human --channel chat — задача
    let o = p.agent(&["idea", "accept", &format!("w:{id}")]);
    assert_eq!(code(&o), Some(6), "{}", err(&o));
    let a = ok(&p.agent(&[
        "idea",
        "accept",
        &format!("w:{id}"),
        "--by",
        "human",
        "--channel",
        "chat",
    ]));
    let task = a
        .trim()
        .rsplit_once("задача k:")
        .map(|(_, t)| t.trim().to_string())
        .unwrap_or_else(|| panic!("{a}"));
    let e = p.get(&id);
    assert_eq!(e["status"], "accepted", "{e}");
    assert_eq!(e["data"]["task_id"], task.as_str(), "{e}");
    assert_eq!(e["data"]["decided"]["channel"], "chat", "{e}");
    assert_eq!(p.get(&qid)["status"], "answered", "ответ закрыл вопрос");
    assert_eq!(p.get(&task)["kind"], "task");
    // повторный accept — код 4 с задачей
    let o = p.z(&["idea", "accept", &id]);
    assert_eq!(code(&o), Some(4), "{}", err(&o));
    assert!(err(&o).contains(&format!("k:{task}")), "{}", err(&o));
    // 403 → код 3
    let other = new_id(&p.z(&["idea", "new", "Голосовой ввод"]));
    let o = p.z(&["idea", "accept", &other, "--by", "agent"]);
    assert_eq!(code(&o), Some(3), "{}", err(&o));
    assert_eq!(p.get(&other)["status"], "new");
    // отсрочка до даты и отказ — как REST
    let o = ok(&p.z(&["idea", "defer", &other, "--until", "2099-03-01"]));
    assert!(o.contains("deferred до 2099-03-01"), "{o}");
    let e = p.get(&other);
    assert_eq!(
        (e["status"].as_str(), e["data"]["horizon"].as_str()),
        (Some("deferred"), Some("2099-03-01"))
    );
    ok(&p.z(&["idea", "decline", &other, "--reason", "не нужно"]));
    assert_eq!(p.get(&other)["status"], "declined");
    // свою идею агент снимает сам
    ok(&p.agent(&["idea", "decline", &mine, "--by", "agent"]));
    assert_eq!(p.get(&mine)["status"], "declined");
}

// ---------- п. 1 через хук ----------

/// §13 п. 1 через `zam hook session-start`: Claude Code и `--agent codex` — раздел `### Идеи` с
/// `w:<id>` в `additionalContext` (каждый агент — своя сессия: показ раз за сессию); повторный
/// старт той же сессии (`compact`) — без раздела.
#[test]
fn acceptance_hook_session_start_shows_idea() {
    let _g = serial();
    let (url, tok) = start("hook");
    let p = Proj::new("hook", &url, &tok, GA);
    let id = new_id(&p.z(&["idea", "new", "Кэш миниатюр", "--origin", "human"]));
    for agent in ["claude", "codex"] {
        let s = format!("hk-{agent}");
        let mut c = String::new();
        // бюджет хука: на занятой машине первый вызов может замолчать по сроку
        for i in 0..5 {
            let ev = p.ev(
                "SessionStart",
                &format!("{s}-{i}"),
                json!({"source": "startup"}),
            );
            let (o, _) = p.hook("session-start", agent, &ev);
            c = context(&o);
            if c.contains(&format!("w:{id}")) {
                let again = p.ev(
                    "SessionStart",
                    &format!("{s}-{i}"),
                    json!({"source": "compact"}),
                );
                let (o, _) = p.hook("session-start", agent, &again);
                let c2 = context(&o);
                assert!(!c2.contains("### Идеи"), "{agent}: повтор с разделом: {c2}");
                break;
            }
        }
        assert!(
            c.contains("### Идеи"),
            "{agent}: нет раздела: {c}\n{}",
            p.log()
        );
        assert!(
            c.contains(&format!(
                "ZeroAIMemory: идея «Кэш миниатюр» [normal] — w:{id}"
            )),
            "{agent}: {c}"
        );
    }
}

// ---------- п. 15 ----------

/// Один замер: p95 `session-start` и `prompt` решения с идеями и без (вперемешку).
fn measure(pa: &Proj, pb: &Proj, round: usize) -> [(Duration, Duration); 2] {
    const N: usize = 40;
    let mut t: [[Vec<Duration>; 2]; 2] = Default::default();
    for i in 0..N {
        for (k, p) in [pa, pb].iter().enumerate() {
            let s = format!("p95-{round}-{k}-{i}");
            let ev = p.ev("SessionStart", &s, json!({"source": "startup"}));
            let (o, d) = p.hook("session-start", "claude", &ev);
            ok(&o);
            t[0][k].push(d);
            let ev = p.ev(
                "UserPromptSubmit",
                &format!("{s}-p"),
                json!({"prompt": format!("как улучшить кэш и деплой, шаг {}", i % 5)}),
            );
            let (o, d) = p.hook("prompt", "claude", &ev);
            ok(&o);
            t[1][k].push(d);
        }
    }
    let [ss, pr] = t;
    let [ss_a, ss_b] = ss;
    let [pr_a, pr_b] = pr;
    [(p95(ss_a), p95(ss_b)), (p95(pr_a), p95(pr_b))]
}

/// §13 п. 15: p95 `zam hook session-start` и `prompt` решения с 50 идеями человека против
/// решения без идей того же сервера (вызовы вперемешку — общий фон нагрузки) — прирост ≤ 5 и ≤
/// 15 мс. Отладочная сборка на занятой машине шумит: до трёх замеров, засчитывается первый
/// уложившийся; все замеры печатаются (`ZAM_TIMING`) для журнала этапа.
#[test]
fn acceptance_recall_p95_with_50_ideas() {
    let _g = serial();
    let (url, tok) = start("p95");
    let pa = Proj::new("p95-a", &url, &tok, GA);
    let pb = Proj::new("p95-b", &url, &tok, GB);
    let topics = ["кэш", "деплой", "поиск", "логирование", "миграции"];
    for i in 0..50 {
        let (code, v) = rest(
            &url,
            &tok,
            "POST",
            &format!("/spaces/{GA}/entries"),
            Some(
                &json!({"kind": "idea", "title": format!("Идея {i}: улучшить {}", topics[i % 5]),
                         "body": "Замысел на будущее.", "data": {"origin": "human"}}),
            ),
        );
        assert_eq!(code, 201, "{v}");
    }
    // прогрев обоих решений (не в замер)
    for (k, p) in [&pa, &pb].iter().enumerate() {
        for i in 0..5 {
            let ev = p.ev(
                "SessionStart",
                &format!("warm-{k}-{i}"),
                json!({"source": "startup"}),
            );
            ok(&p.hook("session-start", "claude", &ev).0);
            let ev = p.ev(
                "UserPromptSubmit",
                &format!("warm-{k}-{i}-p"),
                json!({"prompt": "кэш"}),
            );
            ok(&p.hook("prompt", "claude", &ev).0);
        }
    }
    // раздел идей в дайджесте решения с идеями действительно есть
    let ev = pa.ev("SessionStart", "p95-check", json!({"source": "startup"}));
    let c = context(&pa.hook("session-start", "claude", &ev).0);
    assert!(c.contains("### Идеи"), "{c}\n{}", pa.log());
    let (ss_max, pr_max) = (Duration::from_millis(5), Duration::from_millis(15));
    let mut report = Vec::new();
    let mut passed = false;
    for round in 0..3 {
        let [(ssa, ssb), (pra, prb)] = measure(&pa, &pb, round);
        let line = format!(
            "замер {round}: session-start p95 {ssa:?} с идеями / {ssb:?} без; prompt p95 {pra:?} / \
             {prb:?}"
        );
        eprintln!("ZAM_TIMING m57 {line}");
        report.push(line);
        if ssa.saturating_sub(ssb) <= ss_max && pra.saturating_sub(prb) <= pr_max {
            passed = true;
            break;
        }
    }
    assert!(
        passed,
        "прирост p95 выше порогов (+5 мс session-start, +15 мс prompt):\n{}",
        report.join("\n")
    );
}
