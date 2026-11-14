//! Хуки `zam hook …` (§10.3; задача 10 M4) против настоящего zam-server в том же процессе тестов:
//! вывод по контракту Claude Code и Codex, локальный кэш триггеров (ETag), один показ за сессию,
//! напоминание `checkpoint` (R1), переключатели (R5), fail-open и время. Сервер без моделей
//! (поиск — FTS). Каталоги проекта и «домашний» — во временном каталоге целей сборки
//! (`CARGO_TARGET_TMPDIR`), не в профиле пользователя.
//!
//! **Время (задача 17b-http M6).** Бюджет хука §10.3 отсчитывается от старта процесса `zam`
//! (`started` в `main`), поэтому тесты сверяют с ним время **внутри процесса** ([`Times::inner`]:
//! хук сам пишет его в файл по `ZAM_TEST_HOOK_TIMING`), а не время запуска и выхода отладочного
//! exe глазами теста — под нагрузкой растёт именно оно. Пределы серии ([`assert_series`]):
//! медиана — сам бюджет, p95 — прежний запас R3, ×4 бюджета ([`max_for`]: хвост отладочной сборки
//! на занятом HDD — остановки диска, не хук); лучший из повторов (медленный или выключенный
//! сервер, [`Proj::hook_best`]) — полтора бюджета ([`max_inner`]); одиночный замер, который
//! повторить нельзя, — [`max_for`]; запуск и выход процесса —
//! отдельно, нормировкой по пустому вызову `zam hook` в том же цикле ([`assert_launch`]). Тесты содержимого идут со щедрым бюджетом ([`Proj::hook`], [`hook_slow`]):
//! успеет ли отладочная сборка на занятой машине в 70–270 мс — не свойство содержимого.
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
/// Предел одиночного замера и p95 серии времени хука внутри процесса по команде (R3, fix round
/// 1, m-1): ×4 от бюджета §10.3 — `pre-tool` 200 мс, `checkpoint` 400 мс, `session-start` 1200 мс,
/// `prompt` и `post-tool` 1000 мс. Запас — на остановки диска отладочной сборки (запись строки
/// `hooks.log`, возврат очереди: на занятом HDD одна операция с файлом — до сотен мс); сам бюджет
/// сверяют медиана серии ([`assert_series`]) и лучший из повторов ([`max_inner`]).
fn max_for(kind: &str) -> Duration {
    Duration::from_millis(4 * budget_ms(kind))
}
/// Бюджет команды хука §10.3, мс (`Kind::budget_ms`; у `post-tool` — режима ошибки).
fn budget_ms(kind: &str) -> u64 {
    match kind {
        "checkpoint" => 100,
        "session-start" | "subagent-start" => 300,
        "pre-tool" => 50,
        _ => 250,
    }
}
/// Предел времени хука внутри процесса ([`Times::inner`]) у лучшего из повторов с медленным
/// сервером: полтора бюджета §10.3 (`session-start` 450 мс, `checkpoint` 150 мс). Срок HTTP —
/// бюджет минус 30 мс, но ожидание сокета возвращается с шагом таймера ОС (Windows — до 15,6 мс),
/// а отладочная сборка на HDD после него ещё пишет строки `hooks.log` и возвращает очередь
/// (замер 17b-http при бюджете 300 мс: 283–307 мс на тихой машине, 281–540 мс под чужой сборкой
/// на том же диске). Второй срок запроса сверх первого (дефект 17b-http) давал не меньше 557 мс —
/// 1,86 бюджета — и на тихой машине.
fn max_inner(kind: &str) -> Duration {
    Duration::from_millis(budget_ms(kind) * 3 / 2)
}
/// Бюджет хука тестов содержимого, мс (`ZAM_TEST_HOOK_BUDGET_MS`).
const ROOMY_BUDGET_MS: &str = "10000";
/// Запас нормировки запуска процесса ([`assert_launch`]).
const LAUNCH_SLACK: Duration = Duration::from_millis(30);

/// Медиана.
fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

/// Разброс времени пустого вызова `zam hook` ([`Proj::idle`]) в серии: p95 − минимум. На тихой
/// машине — единицы мс; на занятой (все ядра в работе, очередь к диску) — десятки и сотни: столько
/// машина в этот момент прибавляет процессу, который ничего не делает.
fn jitter(idle: &[Duration]) -> Duration {
    match idle.iter().min() {
        Some(min) => p95(idle.to_vec()).saturating_sub(*min),
        None => Duration::ZERO,
    }
}

/// Серия замеров времени хука `kind` внутри процесса: медиана — не больше бюджета `budget`
/// (§10.3; устойчива к остановкам диска), p95 — не больше `p95_max` (запас R3) и разброса пустого
/// вызова в той же серии ([`jitter`]; `idle` пуст — без него): хвост серии нормируется тем, что
/// занятая машина делает с пустым процессом.
fn assert_series(
    what: &str,
    inner: Vec<Duration>,
    budget: Duration,
    p95_max: Duration,
    idle: &[Duration],
) -> Duration {
    let (q, mid) = (p95(inner.clone()), median(inner));
    assert!(
        mid <= budget,
        "{what}: медиана {mid:?} при бюджете {budget:?}"
    );
    let noise = jitter(idle);
    assert!(
        q <= p95_max + noise,
        "{what}: p95 {q:?} при пределе {p95_max:?} и разбросе пустого вызова {noise:?}"
    );
    q
}

/// Запуск и выход процесса хука (вне бюджета: [`Times::launch`]) — нормировкой по пустому вызову
/// `zam hook` в том же цикле (`idle` — [`Proj::idle`]): медиана не больше двух медиан пустого
/// вызова и [`LAUNCH_SLACK`]. Занятая машина замедляет оба одинаково; хук, который дорого
/// стартует или не выходит (ждёт поток, держит процесс), — только первое.
fn assert_launch(what: &str, hooks: &[Times], idle: &[Duration]) {
    let launch = median(hooks.iter().map(|t| t.launch()).collect());
    let base = median(idle.to_vec());
    eprintln!(
        "ZAM_TIMING {what}: запуск и выход процесса {launch:?} (медиана), пустой вызов {base:?}"
    );
    assert!(
        launch <= base * 2 + LAUNCH_SLACK,
        "{what}: запуск и выход процесса {launch:?} при пустом вызове {base:?}"
    );
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

/// Времена одного вызова хука. `inner` — внутри процесса, от старта `main` до конца вывода (от
/// того же момента отсчитывается бюджет хука, §10.3); `outer` — весь процесс глазами теста:
/// запуск отладочного exe, `inner` и выход. Разность — запуск и выход процесса: под нагрузкой
/// растёт она, а не хук (журнал M6, «Расследование: нестабильные тесты хуков под нагрузкой»).
#[derive(Debug, Clone, Copy)]
struct Times {
    inner: Duration,
    outer: Duration,
}

impl Times {
    /// Запуск и выход процесса (вне бюджета хука).
    fn launch(self) -> Duration {
        self.outer.saturating_sub(self.inner)
    }
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

    /// `zam hook <kind> [--agent codex]` с JSON на stdin, с бюджетом продукта: вывод и время
    /// хука **внутри процесса** ([`Times::inner`]) — с ним сверяются бюджеты.
    fn hook_at(&self, url: &str, kind: &str, agent: &str, stdin: &[u8]) -> (Output, Duration) {
        let (o, t) = self.hook_times(url, kind, agent, stdin);
        (o, t.inner)
    }

    /// [`Proj::hook_at`] для одиночной проверки бюджета, устойчивой к остановке диска: вызов
    /// повторяется (до трёх раз), пока время внутри процесса не уложится в [`max_inner`]; вывод —
    /// первого вызова, время — лучшее. Обычно вызов один. Остановка диска (на занятом HDD одна
    /// операция с файлом — до секунд) случайна и повтором снимается; хук, который в бюджет не
    /// укладывается, не уложится все три раза. Только там, где повтор события безвреден.
    fn hook_best(&self, url: &str, kind: &str, agent: &str, stdin: &[u8]) -> (Output, Duration) {
        let (first, mut best) = self.hook_at(url, kind, agent, stdin);
        for _ in 0..2 {
            if best <= max_inner(kind) {
                break;
            }
            best = best.min(self.hook_at(url, kind, agent, stdin).1);
        }
        (first, best)
    }

    /// [`Proj::hook_at`] с обоими временами вызова ([`Times`]).
    fn hook_times(&self, url: &str, kind: &str, agent: &str, stdin: &[u8]) -> (Output, Times) {
        self.hook_env(url, kind, agent, stdin, &[])
    }

    /// Пустой вызов `zam hook <kind>` (stdin пуст — хук сразу выходит): время процесса целиком —
    /// мера запуска и выхода отладочного exe на этой машине в этот момент ([`assert_launch`]).
    fn idle(&self, kind: &str) -> Duration {
        self.hook_times(DEAD, kind, "claude", b"").1.outer
    }

    /// Хук с переменными окружения `envs` (подмены тестов) и обоими временами вызова. Время внутри
    /// процесса хук пишет сам (тестовый канал `ZAM_TEST_HOOK_TIMING`, свой файл на вызов); строки
    /// нет — в отладочной сборке это падение теста, в release без канала — время снаружи (оно не
    /// меньше).
    fn hook_env(
        &self,
        url: &str,
        kind: &str,
        agent: &str,
        stdin: &[u8],
        envs: &[(&str, &str)],
    ) -> (Output, Times) {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let file = self
            .repo
            .parent()
            .unwrap()
            .join(format!("hook-timing-{n}.txt"));
        let mut c = self.cmd(url);
        c.env("ZAM_TEST_HOOK_TIMING", &file)
            .envs(envs.iter().copied())
            .args(["hook", kind, "--agent", agent])
            .write_stdin(stdin.to_vec());
        let t = Instant::now();
        let o = c.output().unwrap();
        let outer = t.elapsed();
        let inner = std::fs::read_to_string(&file)
            .ok()
            .and_then(|s| {
                let (k, us) = s.lines().last()?.split_once(' ')?;
                (k == kind).then(|| us.trim().parse().ok()).flatten()
            })
            .map(Duration::from_micros);
        let _ = std::fs::remove_file(&file);
        // отладочная сборка: канал обязан сработать — иначе все проверки времени молча вернулись
        // бы к времени снаружи; release без фичи `test-hooks` канала не имеет
        assert!(
            inner.is_some() || !cfg!(debug_assertions) || !o.status.success(),
            "хук {kind} не записал время в {}",
            file.display()
        );
        let inner = inner.unwrap_or(outer);
        (o, Times { inner, outer })
    }

    /// Хук ради содержимого (вывод, кэш, запросы): со щедрым бюджетом ([`ROOMY_BUDGET_MS`]) —
    /// занятая машина не превращает ответ в тишину. Время и бюджеты — [`Proj::hook_at`].
    fn hook(&self, kind: &str, v: &Value) -> Output {
        self.hook_roomy(&self.url, kind, "claude", v).0
    }

    /// [`Proj::hook_at`] ради содержимого: тот же вызов со щедрым бюджетом (время — справочно).
    fn hook_content(&self, url: &str, kind: &str, agent: &str, stdin: &[u8]) -> (Output, Duration) {
        let (o, t) = self.hook_env(
            url,
            kind,
            agent,
            stdin,
            &[("ZAM_TEST_HOOK_BUDGET_MS", ROOMY_BUDGET_MS)],
        );
        (o, t.inner)
    }

    /// [`Proj::hook`] с адресом, агентом и временем внутри процесса (справочно: щедрый бюджет
    /// хук не обрезает).
    fn hook_roomy(&self, url: &str, kind: &str, agent: &str, v: &Value) -> (Output, Duration) {
        self.hook_content(url, kind, agent, v.to_string().as_bytes())
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
    let (mut times, mut idle) = (Vec::new(), Vec::new());
    for i in 0..100 {
        let v = write_ev(&p, &format!("t{i}"), "scripts/deploy.sh").to_string();
        let (o, t) = p.hook_times(DEAD, "pre-tool", "claude", v.as_bytes());
        assert!(context(&o, "PreToolUse").contains("i:E1"));
        times.push(t);
        idle.push(p.idle("pre-tool"));
    }
    let inner: Vec<Duration> = times.iter().map(|t| t.inner).collect();
    let q = p95(inner.clone());
    let min = inner.iter().min().unwrap();
    eprintln!(
        "ZAM_TIMING pre-tool внутри процесса p95={q:?} min={min:?} (100 запусков); процесс целиком p95={:?}",
        p95(times.iter().map(|t| t.outer).collect())
    );
    // бюджет `pre-tool` §10.3 — 50 мс от старта процесса: медиана — в бюджете, p95 — ×4
    let budget = Duration::from_millis(budget_ms("pre-tool"));
    assert_series("pre-tool", inner, budget, max_for("pre-tool"), &idle);
    assert_launch("pre-tool", &times, &idle);
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
///
/// Две части (ревью 17b-http). Настоящая уборка 3000 файлов: привязка — новой сессии, бюджет
/// запросов цел; время одиночного замера не сверяется — на занятом HDD уборка и записи хука
/// делят один каталог, и хук выходил на 1,6–3,4 с без всякого ожидания уборки. Суть — подменой
/// `ZAM_TEST_GC_STALL_MS`: уборка «идёт» 30 с, а привязка записана (значит, до уборки), и хук
/// вышел на порядок раньше её конца (значит, не ждал).
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
    let (o, _) = p.hook_at(&url, "session-start", "claude", ev.to_string().as_bytes());
    assert!(o.status.success());
    assert_eq!(binding_session(&p).as_deref(), Some("ss-big"));
    let log = p.log();
    assert!(!log.contains(zam_core::HOOK_BUDGET_SPENT), "{log}");
    // уборка занята 30 с: привязка новой сессии того же агента (`clear`) уже записана, хук
    // уборку не ждал (ждал бы — шёл бы 30 с; запись привязки после уборки — хук ждал бы её до
    // потолка 1,5 с и вышел со старой привязкой и строкой в журнале)
    let ev = p.ev("SessionStart", "ss-big-2", json!({"source": "clear"}));
    let (o, t) = p.hook_env(
        &url,
        "session-start",
        "claude",
        ev.to_string().as_bytes(),
        &[("ZAM_TEST_GC_STALL_MS", STALL_MS)],
    );
    assert!(o.status.success());
    let log = p.log();
    assert_eq!(binding_session(&p).as_deref(), Some("ss-big-2"), "{log}");
    assert!(!log.contains("привязка MCP к сессии"), "{log}");
    assert!(
        t.inner <= LATE_THREAD_MAX,
        "хук ждал уборку кэша: {:?} при паузе уборки 30 с: {log}",
        t.inner
    );
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
    let (o, d) = p.hook_best(DEAD, "post-tool", "claude", ev.to_string().as_bytes());
    let c = context(&o, "PostToolUseFailure");
    assert!(
        c.contains("эта ошибка уже разбиралась: CRLF в shebang [gotcha·th] i:E3"),
        "{c}"
    );
    assert!(d <= max_inner("post-tool"), "{d:?}");
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
    let (o, _) = p.hook_content(
        &url,
        "post-tool",
        "codex",
        ev("/bin/bash^M: bad interpreter").to_string().as_bytes(),
    );
    let c = context(&o, "PostToolUse");
    assert!(c.contains(&format!("i:{id}")), "{c}");
    let (o, _) = p.hook_content(&url, "post-tool", "codex", ev("").to_string().as_bytes());
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
        let (o, d) = p.hook_best(DEAD, k, "claude", v.to_string().as_bytes());
        empty(&o);
        assert!(d <= max_inner(k), "{k}: {d:?}");
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
        // формат — со щедрым бюджетом (содержимое); время — отдельным вызовом с бюджетом продукта
        let (o, _) = p.hook_roomy(&url, "session-start", agent, &ss);
        let s = ok(&o);
        assert_eq!(s.lines().count(), 1, "{s}");
        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v.as_object().unwrap().len(), 1, "{v}");
        assert_eq!(v["hookSpecificOutput"].as_object().unwrap().len(), 2, "{v}");
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
        let (o, d) = p.hook_best(&url, "session-start", agent, ss.to_string().as_bytes());
        ok(&o);
        assert!(d <= max_inner("session-start"), "{agent}: {d:?}");
    }
    // prompt: подсказка (повтор в той же сессии сервер гасит — каждый раз новая сессия)
    let mut found = None;
    for i in 0..5 {
        let pr = p.ev(
            "UserPromptSubmit",
            &format!("op{i}"),
            json!({"prompt": "почему квазар светит"}),
        );
        let (o, d) = p.hook_roomy(&url, "prompt", "claude", &pr);
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

/// R3: время `session-start`, `prompt`, `post-tool` и `checkpoint` с сервером — 20 запусков
/// каждого, **внутри процесса** ([`assert_series`]): медиана — не больше бюджета §10.3 (300, 250,
/// 250 и 100 мс), p95 — не больше ×4 от него (1200, 1000, 1000 и 400 мс; прежде — только p95 и
/// на весь процесс); запуск и выход процесса — отдельно, нормировкой по пустому вызову
/// ([`assert_launch`]); пустых ответов (хук замолчал по
/// внутреннему сроку, fail-open) — не больше 5 % всех 80 вызовов, как у
/// `m4_acceptance::hook_budgets`: на общем раннере CI отладочная сборка изредка не укладывается
/// в бюджет (ubuntu: 872cfc4, e32fe1f, d020f16; на стенде A/B p95 ~15 мс без пустых — журнал
/// M5, «Финальное ревью ветки»).
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
        let (mut times, mut idle) = (Vec::new(), Vec::new());
        for i in 0..20 {
            if k == "checkpoint" {
                p.hook("pre-tool", &write_ev(&p, &format!("tm{i}"), "a.txt"));
            }
            let (o, t) = p.hook_times(&url, k, "claude", mk(i).to_string().as_bytes());
            calls += 1;
            if ok(&o).trim().is_empty() {
                silent.push(format!("{k} #{i} {:?}", t.inner));
            }
            times.push(t);
            idle.push(p.idle(k));
        }
        let q = assert_series(
            k,
            times.iter().map(|t| t.inner).collect(),
            Duration::from_millis(budget_ms(k)),
            max_for(k),
            &idle,
        );
        report.push(format!(
            "{k} внутри процесса p95={q:?}, процесс целиком p95={:?}",
            p95(times.iter().map(|t| t.outer).collect())
        ));
        assert_launch(k, &times, &idle);
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
    let (mut times, mut idle) = (Vec::new(), Vec::new());
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
        let (o, t) = p.hook_times(DEAD, "post-tool", "claude", v.to_string().as_bytes());
        if tool == "Read" {
            assert_eq!(context(&o, "PostToolUse"), line);
        } else {
            empty(&o);
        }
        times.push(t);
        idle.push(p.idle("post-tool"));
    }
    // правки с символьной ссылкой: в очереди — хэши символов (или строка без них по отсечке)
    let q = queue(&p, "pt1");
    assert_eq!(q.len(), 1, "{q:?}");
    eprintln!(
        "ZAM_TIMING post-tool: символов в очереди {}",
        q[0]["symbols"].as_array().map_or(0, Vec::len)
    );
    let inner: Vec<Duration> = times.iter().map(|t| t.inner).collect();
    let q = p95(inner.clone());
    let min = inner.iter().min().unwrap();
    eprintln!(
        "ZAM_TIMING post-tool (успех) внутри процесса p95={q:?} min={min:?} (100 запусков); процесс целиком p95={:?}",
        p95(times.iter().map(|t| t.outer).collect())
    );
    // успешный `post-tool` — без сети, 50 мс от старта процесса (M4.5 §9.1), как `pre-tool`:
    // медиана — в бюджете, p95 — ×4
    let budget = Duration::from_millis(budget_ms("pre-tool"));
    assert_series(
        "post-tool (успех)",
        inner,
        budget,
        max_for("pre-tool"),
        &idle,
    );
    assert_launch("post-tool (успех)", &times, &idle);
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
/// Тест содержимого: хуки с живым сервером идут со щедрым бюджетом ([`hook_slow`]) — на занятой
/// машине отладочная сборка не доходит до запроса за 70–220 мс (запуск потока — до кванта
/// планировщика, каждая операция с файлом очереди — десятки мс), и это свойство машины, а не
/// порядка отправки; бюджеты меряют `m4_acceptance::hook_budgets` (release) и тесты времени.
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
    ok(&hook_slow(&p, &url, "session-start", &ss));
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
    let o = hook_slow(&p, &url, "post-tool", &read);
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
    ok(&hook_slow(&p, &url, "post-tool", &edit));
    assert_eq!(queue(&p, "fl").len(), 1);
    let pr = p.ev("UserPromptSubmit", "fl", json!({"prompt": "что с пулом"}));
    ok(&hook_slow(&p, &url, "prompt", &pr));
    assert!(
        queue(&p, "fl").is_empty(),
        "очередь отправлена: {}",
        p.log()
    );
    assert_eq!(ref_state(&file_refs_index(&p), "src/pool.rs"), ["stale"]);
    // сервер недоступен — очередь остаётся до следующего раза
    std::fs::write(p.repo.join("src/pool.rs"), b"v1\n").unwrap();
    ok(&hook_slow(&p, &url, "post-tool", &edit));
    // недоступный сервер — с бюджетом продукта: отказ соединения ждать дольше незачем
    let _ = p.hook_at(DEAD, "prompt", "claude", pr.to_string().as_bytes());
    assert_eq!(queue(&p, "fl").len(), 1, "ошибка — очередь цела");
    let stop = p.ev("Stop", "fl", json!({"stop_hook_active": false}));
    ok(&hook_slow(&p, &url, "checkpoint", &stop));
    assert!(
        queue(&p, "fl").is_empty(),
        "checkpoint отправил: {}",
        p.log()
    );
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
    // содержимое («один запрос на все очереди») — со щедрым бюджетом; бюджет — во второй части
    ok(&hook_slow(&p, &url, "session-start", &ss));
    let s = seen.lock().unwrap().clone();
    let posts: Vec<&Value> = s
        .iter()
        .filter(|(m, path, _)| m == "POST" && path == "/api/v1/files/changed")
        .map(|(_, _, b)| b)
        .collect();
    // пропуск отправки по остатку бюджета виден в журнале хука («files/changed: очередь не
    // отправлена — … (прошло N мс)»)
    assert_eq!(posts.len(), 1, "журнал хука: {:?}; запросы: {s:?}", p.log());
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
    // лучший из повторов — полтора бюджета (повтор — только если время не уложилось: после срока
    // запроса хук ещё возвращает очередь, а создание файла на занятом диске изредка стоит секунды)
    let posts = || {
        slow_seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, path, _)| path == "/api/v1/files/changed")
            .count()
    };
    let (mut attempts, mut d) = (0, Duration::MAX);
    while attempts < 3 && d > max_inner("session-start") {
        let before = posts();
        let (_, t) = p.hook_at(&slow, "session-start", "claude", ss.to_string().as_bytes());
        attempts += 1;
        d = d.min(t);
        // прирост — этого хука: счёт берётся, когда заглушка дочитала принятое (два равных
        // чтения через 50 мс), иначе опоздавшая запись ушла бы в счёт следующей попытки
        let mut now = posts();
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(50));
            let next = posts();
            if next == now {
                break;
            }
            now = next;
        }
        let sent = now - before;
        assert!(
            sent <= 1,
            "не больше одного запроса на хук: {sent} (попытка {attempts}): {}",
            p.log()
        );
    }
    assert!(
        d <= max_inner("session-start"),
        "session-start {d:?}: {}",
        p.log()
    );
    let left: usize = (0..5).map(|i| queue(&p, &format!("s{i}")).len()).sum();
    assert_eq!(left, 5, "тайм-аут — очереди целы");
}

/// Задача 17b-http: срок HTTP хука — абсолютный (старт процесса + бюджет − резерв), один на все
/// запросы хука. Заглушка молчит 3 с на каждый запрос: `session-start` шлёт `POST /files/changed`
/// и следом, тем же клиентом, `GET /file-refs`. Со сроком «от начала запроса» второй запрос
/// получал ещё один полный срок — 600–1100 мс при бюджете 300 мс; с абсолютным — не начинается
/// вовсе (одна строка в `hooks.log`), и хук выходит в свой бюджет.
///
/// Две проверки. Без времени: `GET /file-refs` до заглушки не доходит ни разу — отправка очереди
/// кончилась сроком, значит, остатка у второго запроса нет. По времени **внутри процесса**
/// ([`Times::inner`]) с бюджетом продукта: лучший из трёх — полтора бюджета ([`max_inner`]; двойной
/// срок давал 557–2464 мс). Худший не сверяется: после срока запроса хук ещё возвращает очередь
/// (создание файла), а на занятом диске это изредка стоит секунду и больше.
#[test]
fn session_start_slow_server_one_deadline_for_all_requests() {
    let _g = serial();
    let (slow, seen) = common::stub(200, r#"{"stale":0}"#, Duration::from_secs(3));
    let p = Proj::new("deadline-seq", &slow, "zam_x");
    // прогрев процесса (первый запуск отладочного exe — с диска)
    let _ = p.hook_at(
        DEAD,
        "pre-tool",
        "claude",
        write_ev(&p, "d-warm", "a.txt").to_string().as_bytes(),
    );
    let mut times = Vec::new();
    for i in 0..3 {
        put_queue(&p, &format!("dq{i}"), &[(format!("src/f{i}.rs"), 'b')]);
        let ss = p.ev(
            "SessionStart",
            &format!("d-new{i}"),
            json!({"source": "startup"}),
        );
        let (o, t) = p.hook_times(&slow, "session-start", "claude", ss.to_string().as_bytes());
        empty(&o);
        times.push(t);
    }
    eprintln!("ZAM_TIMING session-start, медленный сервер: {times:?}");
    let sent = seen.lock().unwrap().clone();
    // путь у заглушки — со строкой запроса (`/file-refs?solution=…`)
    let count = |method: &str, path: &str| {
        sent.iter()
            .filter(|(m, q, _)| m == method && q.split('?').next() == Some(path))
            .count()
    };
    let log = p.log();
    let posts = count("POST", "/api/v1/files/changed");
    assert!(posts >= 1, "отправка очереди ушла: {sent:?}; {log}");
    // Отправка очереди ушла и кончилась сроком — второго запроса потока у этого хука нет. Хук, не
    // успевший до отправки (до срока оставалось меньше 15 мс — `FLUSH_MIN_LEFT`), очередь не шлёт,
    // а `GET /file-refs` с остатком в 1–15 мс начать вправе: запрос уходит и снимается сроком
    // (строка «… не обновлён: сервер … timeout» — правда о нём). Поэтому: запросов `GET` — не
    // больше, чем хуков без отправки; строка «сервер» запрещена, только когда отправка была у всех
    let gets = count("GET", "/api/v1/file-refs");
    assert!(
        gets <= 3 - posts,
        "запрос после срока хука: GET {gets} при отправках {posts}: {sent:?}; {log}"
    );
    if posts == 3 {
        let unsent = format!("кэш file-refs не обновлён: {}", zam_core::HOOK_BUDGET_SPENT);
        assert_eq!(log.matches(&unsent).count(), 3, "одна строка на хук: {log}");
        assert!(!log.contains("file-refs не обновлён: сервер"), "{log}");
    }
    let inner: Vec<Duration> = times.iter().map(|t| t.inner).collect();
    let best = *inner.iter().min().unwrap();
    assert!(
        best <= max_inner("session-start"),
        "session-start внутри процесса {best:?} при бюджете 300 мс: {times:?}\n{log}"
    );
    // срок — очереди целы
    let left: usize = (0..3).map(|i| queue(&p, &format!("dq{i}")).len()).sum();
    assert_eq!(left, 3, "{log}");
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
    let _ = p.hook_content(&url, "session-start", "claude", ss.to_string().as_bytes());
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
    let (o, _) = p.hook_content(
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
/// задачу); очередь другого типа не трогается; без `agent_id` — пакет в своей сессии субагента.
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
    // задача 19 M6: пакет — в своей сессии субагента с parent_id родителя
    assert_eq!(t19_children(&p, "sg2").len(), 1);
}

/// Сервер недоступен — тишина и код 0; событие `subagent` в журнале есть, запись очереди взята.
#[test]
fn subagent_start_server_down_silent() {
    let _g = serial();
    let p = Proj::new("subagent-down", DEAD, "zam_x");
    pre_dead(&p, &spawn_ev(&p, "sd", "Agent", Some("Plan"), "пульсар"));
    assert_eq!(spawns(&p, "sd").len(), 1);
    let (o, d) = p.hook_best(
        DEAD,
        "subagent-start",
        "claude",
        sub_ev(&p, "sd", "a1", "Plan").to_string().as_bytes(),
    );
    empty(&o);
    assert!(d <= max_inner("subagent-start"), "{d:?}");
    assert!(spawns(&p, "sd").is_empty());
    let ev = events(&p, "sd");
    assert!(ev.iter().any(|e| e["ev"] == "subagent"), "{ev:?}");
}

/// Бюджет 300 мс: с живым сервером внутри процесса медиана — в бюджете, p95 ≤ 1200 мс (×4, как
/// `session-start`; [`assert_series`]); сервер, который принимает соединение и молчит, — тишина
/// не дольше полутора бюджетов (лучший из повторов).
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
    let q = assert_series(
        "subagent-start",
        times,
        Duration::from_millis(budget_ms("subagent-start")),
        max_for("subagent-start"),
        &[],
    );
    eprintln!("ZAM_TIMING subagent-start внутри процесса p95={q:?}");
    // «чёрная дыра»: соединение принято, ответа нет
    let hole = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = format!("http://{}", hole.local_addr().unwrap());
    std::thread::spawn(move || {
        let mut keep = Vec::new();
        for s in hole.incoming().flatten() {
            keep.push(s);
        }
    });
    let (o, d) = p.hook_best(
        &addr,
        "subagent-start",
        "claude",
        sub_ev(&p, "sb-hole", "a1", "Plan").to_string().as_bytes(),
    );
    empty(&o);
    assert!(d <= max_inner("subagent-start"), "{d:?}");
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
    let (o, _) = p.hook_content(
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
    let (o, _) = p.hook_content(&url, "pre-tool", "claude", push.to_string().as_bytes());
    want += approx_tokens(&context(&o, "PreToolUse"));
    // тишина — не выдача
    let quiet = p.ev(
        "PreToolUse",
        "tk",
        json!({"tool_name": "Bash", "tool_input": {"command": "ls"}}),
    );
    let (o, _) = p.hook_content(&url, "pre-tool", "claude", quiet.to_string().as_bytes());
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
    let _ = p.hook_content(&url, "prompt", "claude", pr.to_string().as_bytes());
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
    spawn_parts(p).into_iter().map(|x| x.total).collect()
}

/// Время запуска фоновой синхронизации по частям, мс (строка `hooks.log` «запущена за T мс
/// (проверки C мс, процесс P мс, отметка W мс)»): проверки, создание процесса, запись отметки
/// запуска `code-auto.json`.
#[derive(Debug, Clone, Copy)]
struct SpawnParts {
    total: f64,
    checks: f64,
    process: f64,
    mark: f64,
}

fn spawn_parts(p: &Proj) -> Vec<SpawnParts> {
    p.log()
        .lines()
        .filter_map(|l| {
            let rest = l.split("фоновая синхронизация карты запущена за ").nth(1)?;
            let num = |after: &str| -> Option<f64> {
                let r = if after.is_empty() {
                    rest
                } else {
                    rest.split(after).nth(1)?
                };
                r.split_whitespace().next()?.parse().ok()
            };
            Some(SpawnParts {
                total: num("")?,
                checks: num("(проверки ")?,
                process: num(", процесс ")?,
                mark: num(", отметка ")?,
            })
        })
        .collect()
}

/// p95 чисел.
fn p95_f(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[(v.len() * 95).div_ceil(100) - 1]
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
    let (mut hook_times, mut idle) = (Vec::new(), Vec::new());
    for i in 0..20 {
        std::fs::write(p.repo.join(".git/HEAD"), format!("{}\n", sha(200 + i))).unwrap();
        let (o, d) = start(&format!("t{i}"));
        empty(&o);
        hook_times.push(d);
        idle.push(p.idle("session-start"));
    }
    let t: Vec<SpawnParts> = spawn_parts(&p).into_iter().skip(4).collect();
    assert_eq!(t.len(), 20, "{}", p.log());
    // запуск — проверки и создание процесса: то, чего хук «не ждёт»; отметка `code-auto.json` —
    // запись файла кэша, как любая другая у хука
    let launch = p95_f(t.iter().map(|x| x.checks + x.process).collect());
    let mut marks: Vec<f64> = t.iter().map(|x| x.mark).collect();
    marks.sort_by(f64::total_cmp);
    let mark_p50 = marks[marks.len() / 2];
    eprintln!(
        "ZAM_TIMING запуск фоновой синхронизации p95={:.2} мс (проверки и процесс p95={launch:.2} мс, \
         отметка p50={mark_p50:.2} мс, max={:.2} мс); хук внутри процесса p95={:?}",
        p95_f(t.iter().map(|x| x.total).collect()),
        marks[marks.len() - 1],
        p95(hook_times.clone())
    );
    // ≤ 10 мс (§10) — у release; отладочная сборка — ×4 (замер release — в журнале этапа). По
    // p95 — проверки и создание процесса. Отметка — по медиане: время запуска двухмодальное
    // (≈ 6 мс и 45–120 мс у 10–15 % запусков), и второй горб — создание временного файла отметки
    // на занятом диске, а не процесс (замер 17b-http M6: та же запись без запуска процесса даёт
    // тот же хвост; создание процесса — p95 3 мс); p95 из 20 попадал в него
    // Хвост создания процесса нормируется разбросом пустого вызова `zam hook` в той же серии
    // ([`jitter`]): когда все ядра заняты, `CreateProcess` стоит сотни мс и тесту, и хуку
    let max = if cfg!(debug_assertions) { 40.0 } else { 10.0 };
    let noise = jitter(&idle).as_secs_f64() * 1000.0;
    assert!(
        launch <= max + noise,
        "запуск p95 {launch} мс при разбросе пустого вызова {noise:.2} мс: {t:?}"
    );
    assert!(mark_p50 <= max, "отметка запуска p50 {mark_p50} мс: {t:?}");
    // хук не ждёт фоновый процесс: бюджет `session-start` внутри процесса
    assert_series(
        "session-start с запуском синхронизации",
        hook_times,
        Duration::from_millis(budget_ms("session-start")),
        max_for("session-start"),
        &idle,
    );
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
    // бюджет хука — щедрый (содержимое): у постоянной ошибки — 10 с, у медленной второй пачки
    // (ответ через 30 с) — 2 с: первая пачка успевает и на занятой машине, вторая — нет
    for (name, code, delay, budget) in [
        ("flush-part-500", 500u16, Duration::ZERO, ROOMY_BUDGET_MS),
        ("flush-part-slow", 200, Duration::from_secs(30), "2000"),
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
        let _ = p.hook_env(
            &url,
            "session-start",
            "claude",
            ss.to_string().as_bytes(),
            &[("ZAM_TEST_HOOK_BUDGET_MS", budget)],
        );
        let posts: Vec<Value> = seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, path, _)| m == "POST" && path == "/api/v1/files/changed")
            .map(|(_, _, b)| b.clone())
            .collect();
        assert!(
            posts.len() >= 2,
            "{name}: пачек {}: {}",
            posts.len(),
            p.log()
        );
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

/// Бюджет хука вышел **после** разбора очереди (замки, rename, чтение — на занятом диске десятки
/// мс; подмена `ZAM_TEST_FLUSH_STALL_MS`): запроса нет, очередь возвращена на место, причина —
/// строкой в `hooks.log` (раньше эта ветка молчала: «очередь не ушла» без единой строки);
/// следующий хук очередь отправляет.
#[test]
fn changed_flush_budget_spent_after_take_is_logged() {
    let _g = serial();
    let (url, seen) = common::stub(200, r#"{"stale":0,"text":""}"#, Duration::ZERO);
    let p = Proj::new("flush-stall", &url, "zam_x");
    let s = "fs-1";
    put_queue(&p, s, &[("src/a.rs".into(), 'a'), ("src/b.rs".into(), 'b')]);
    let pr = p.ev("UserPromptSubmit", s, json!({"prompt": ""}));
    let posts = || {
        seen.lock()
            .unwrap()
            .iter()
            .filter(|(m, path, _)| m == "POST" && path == "/api/v1/files/changed")
            .count()
    };
    // щедрый бюджет (3 с) — до очереди хук доходит при любой занятости машины; пауза дольше его
    let o = p
        .cmd(&url)
        .env("ZAM_TEST_HOOK_BUDGET_MS", "3000")
        .env("ZAM_TEST_FLUSH_STALL_MS", "3100")
        .args(["hook", "prompt", "--agent", "claude"])
        .write_stdin(pr.to_string().into_bytes())
        .output()
        .unwrap();
    empty(&o);
    let log = p.log();
    assert_eq!(posts(), 0, "{log}");
    assert_eq!(queue(&p, s).len(), 2, "очередь возвращена: {log}");
    assert_eq!(
        log.matches("files/changed: очередь не отправлена — остаток бюджета меньше 15 мс")
            .count(),
        1,
        "{log}"
    );
    assert!(
        log.contains("очередь разобрана, возвращено путей: 2)"),
        "{log}"
    );
    // без паузы — тот же хук очередь отправляет
    let o = p
        .cmd(&url)
        .env("ZAM_TEST_HOOK_BUDGET_MS", "3000")
        .args(["hook", "prompt", "--agent", "claude"])
        .write_stdin(pr.to_string().into_bytes())
        .output()
        .unwrap();
    empty(&o);
    assert_eq!(posts(), 1, "{}", p.log());
    assert!(queue(&p, s).is_empty(), "{}", p.log());
}

/// Файлы очередей изменённых файлов проекта (`changed-*.jsonl`) и забранные, но не возвращённые
/// (`.changed-*.tmp`).
fn changed_files(p: &Proj) -> (Vec<PathBuf>, usize) {
    let mut queues = Vec::new();
    let mut tmp = 0;
    for e in std::fs::read_dir(p.cache()).unwrap().flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if n.starts_with("changed-") && n.ends_with(".jsonl") {
            queues.push(e.path());
        } else if n.starts_with(".changed-") && n.ends_with(".tmp") {
            tmp += 1;
        }
    }
    queues.sort();
    (queues, tmp)
}

/// Задача 17d (пункт d): сервер долго недоступен — очереди изменённых файлов копятся у каждой
/// сессии, а `session-start` отправляет очереди всех сессий проекта. Один хук забирает не больше
/// `FLUSH_QUEUES_MAX` (8) очередей, старые первыми: иначе разбор (замок, rename, чтение на
/// очередь) съедал бы бюджет до запроса — очереди не уходили бы и при ожившем сервере, а возврат
/// шёл бы после срока (замер: +8 мс на очередь, 540 мс при 32 очередях и бюджете 300).
/// Остальные уходят следующими хуками, ничего не теряется — в том числе у очередей старше часа
/// (содержимое читается до того, как очередь становится временным файлом).
#[test]
fn session_start_takes_bounded_number_of_changed_queues() {
    let _g = serial();
    let (url, seen) = common::stub(200, r#"{"stale":0,"text":""}"#, Duration::ZERO);
    let p = Proj::new("flush-cap", &url, "zam_x");
    for i in 0..20 {
        put_queue(
            &p,
            &format!("cap-{i:02}"),
            &[(format!("src/f{i:02}.rs"), 'a')],
        );
        // время файла — порядок «старые первыми» (шаг больше точности времени файловой системы).
        // Все очереди старше часа (`TMP_TTL`): забранный файл наследует время очереди, и уборка
        // кэша, идущая у `session-start` параллельно, удаляла его как брошенный временный до
        // чтения — строки терялись молча (найдено этим тестом: 7 путей из 8)
        let f = p
            .session_file("changed", &format!("cap-{i:02}"), ".jsonl")
            .unwrap();
        let t = std::time::SystemTime::now() - Duration::from_secs(3 * 3600 - i as u64 * 60);
        std::fs::File::options()
            .write(true)
            .open(&f)
            .unwrap()
            .set_modified(t)
            .unwrap();
    }
    let posted = || -> Vec<String> {
        seen.lock()
            .unwrap()
            .iter()
            .filter(|(m, path, _)| m == "POST" && path == "/api/v1/files/changed")
            .flat_map(|(_, _, b)| {
                b["files"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|f| f["path"].as_str().unwrap().to_string())
                    .collect::<Vec<_>>()
            })
            .collect()
    };
    let start = |n: usize| {
        let ev = p.ev(
            "SessionStart",
            &format!("cap-new-{n}"),
            json!({"source": "startup"}),
        );
        p.hook("session-start", &ev);
    };
    start(0);
    // первый хук: восемь самых старых очередей одним запросом
    let mut got = posted();
    got.sort();
    let want: Vec<String> = (0..8).map(|i| format!("src/f{i:02}.rs")).collect();
    assert_eq!(
        got,
        want,
        "{}
{:?}",
        p.log(),
        seen.lock().unwrap()
    );
    let (left, tmp) = changed_files(&p);
    assert_eq!((left.len(), tmp), (12, 0), "{}", p.log());
    // следующие хуки добирают остальное: всё отправлено ровно по разу
    start(1);
    start(2);
    let mut got = posted();
    got.sort();
    let want: Vec<String> = (0..20).map(|i| format!("src/f{i:02}.rs")).collect();
    assert_eq!(got, want, "{}", p.log());
    assert_eq!(changed_files(&p), (Vec::new(), 0), "{}", p.log());
}

/// Задача 17d (пункт d): запрос не состоялся (сервер молчит) — забранные очереди возвращаются на
/// место **как были**, без замка, чтения и переписывания (жёсткая ссылка): возврат идёт уже
/// после срока HTTP, и переписывание каждой очереди через временный файл выводило хук за бюджет.
/// Признак «не переписана» — байты очереди с нестандартной разметкой JSON те же (переписывание
/// сериализует строки заново). Строка, дописанная во время запроса в новую очередь, не теряется
/// (ссылка поверх неё не ставится — обычный возврат слиянием).
#[test]
fn unsent_changed_queue_returns_untouched() {
    let _g = serial();
    let (slow, _seen) = common::stub(200, r#"{"stale":0}"#, Duration::from_secs(3));
    let p = Proj::new("flush-link", &slow, "zam_x");
    std::fs::create_dir_all(p.cache()).unwrap();
    let raw = |i: usize| {
        format!(
            "{{ \"path\":   \"src/l{i}.rs\",\"hash\": \"{}\" }}\n",
            "c".repeat(64)
        )
    };
    let names: Vec<String> = (0..3).map(|i| format!("lk-{i}")).collect();
    for (i, s) in names.iter().enumerate() {
        put_queue(&p, s, &[("x".into(), 'a')]);
        let f = p.session_file("changed", s, ".jsonl").unwrap();
        std::fs::write(&f, raw(i)).unwrap();
        // очередь двухдневной давности
        std::fs::File::options()
            .write(true)
            .open(&f)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - Duration::from_secs(48 * 3600))
            .unwrap();
    }
    let ev = p.ev("SessionStart", "lk-new", json!({"source": "startup"}));
    // бюджет 1,5 с при ответе через 3 с: запрос заведомо начат и снят сроком
    let (o, _) = p.hook_env(
        &slow,
        "session-start",
        "claude",
        ev.to_string().as_bytes(),
        &[("ZAM_TEST_HOOK_BUDGET_MS", "1500")],
    );
    empty(&o);
    let log = p.log();
    assert!(
        log.contains("files/changed:") && !log.contains("очередь не отправлена"),
        "запрос очереди не начат: {log}"
    );
    let (left, tmp) = changed_files(&p);
    assert_eq!((left.len(), tmp), (3, 0), "{log}");
    for (i, s) in names.iter().enumerate() {
        let f = p.session_file("changed", s, ".jsonl").unwrap();
        assert_eq!(
            std::fs::read_to_string(&f).unwrap(),
            raw(i),
            "очередь {s} переписана: {log}"
        );
        // время изменения обновлено: очередь, которую пытаются отправить, уборка кэша не
        // считает брошенной (прежде её освежало переписывание)
        let age = std::fs::metadata(&f)
            .unwrap()
            .modified()
            .unwrap()
            .elapsed()
            .unwrap_or_default();
        assert!(
            age < Duration::from_secs(600),
            "очередь {s}: время изменения не обновлено ({age:?})"
        );
    }
    assert!(!log.contains("возврат очереди"), "{log}");
}

/// Время изменения очереди `changed-<сессия>.jsonl` — `secs` секунд назад.
fn age_queue(p: &Proj, session: &str, secs: u64) {
    let f = p.session_file("changed", session, ".jsonl").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&f)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - Duration::from_secs(secs))
        .unwrap();
}

/// Пути всех принятых заглушкой `POST /files/changed`, по порядку.
fn changed_posted(seen: &common::Seen) -> Vec<String> {
    let mut out: Vec<String> = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|(m, path, _)| m == "POST" && path == "/api/v1/files/changed")
        .flat_map(|(_, _, b)| {
            b["files"]
                .as_array()
                .unwrap()
                .iter()
                .map(|f| f["path"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        })
        .collect();
    out.sort();
    out
}

/// Ревью 17d, Minor 1 (откат D3 — детерминированно): очередь старше часа забирается на отправку
/// rename, и забранный файл наследует её время — для уборки кэша это «брошенный временный
/// файл». Уборка идёт через 100 мс после старта (`ZAM_TEST_GC_STALL_MS`), запрос — через 400 мс
/// после разбора очередей (`ZAM_TEST_FLUSH_STALL_MS`): уборка заведомо проходит по каталогу,
/// пока очереди забраны. Все пути отправлены: содержимое прочитано до rename, а время файла
/// освежено. До правки очередь читалась после паузы — уже удалённой, и строки терялись молча.
#[test]
fn old_changed_queue_survives_cache_gc_during_flush() {
    let _g = serial();
    let (url, seen) = common::stub(200, r#"{"stale":0,"text":""}"#, Duration::ZERO);
    let p = Proj::new("flush-gc", &url, "zam_x");
    for i in 0..3 {
        let s = format!("gc-{i}");
        put_queue(&p, &s, &[(format!("src/g{i}.rs"), 'a')]);
        age_queue(&p, &s, 3 * 3600);
    }
    let ev = p.ev("SessionStart", "gc-new", json!({"source": "startup"}));
    let (o, _) = p.hook_env(
        &url,
        "session-start",
        "claude",
        ev.to_string().as_bytes(),
        &[
            ("ZAM_TEST_HOOK_BUDGET_MS", "3000"),
            ("ZAM_TEST_GC_STALL_MS", "100"),
            ("ZAM_TEST_FLUSH_STALL_MS", "400"),
        ],
    );
    ok(&o);
    assert_eq!(
        changed_posted(&seen),
        ["src/g0.rs", "src/g1.rs", "src/g2.rs"],
        "{}",
        p.log()
    );
    assert_eq!(changed_files(&p), (Vec::new(), 0), "{}", p.log());
}

/// Ревью 17d, Minor 3: время забранной очереди не удалось освежить (`ZAM_TEST_TOUCH_FAIL`) —
/// одна строка в `hooks.log` на хук, а не тишина; очереди при этом отправлены.
#[test]
fn changed_queue_touch_failure_is_logged_once() {
    let _g = serial();
    let (url, seen) = common::stub(200, r#"{"stale":0,"text":""}"#, Duration::ZERO);
    let p = Proj::new("flush-touch", &url, "zam_x");
    for i in 0..3 {
        put_queue(&p, &format!("tf-{i}"), &[(format!("src/t{i}.rs"), 'a')]);
    }
    let ev = p.ev("SessionStart", "tf-new", json!({"source": "startup"}));
    let (o, _) = p.hook_env(
        &url,
        "session-start",
        "claude",
        ev.to_string().as_bytes(),
        &[
            ("ZAM_TEST_HOOK_BUDGET_MS", ROOMY_BUDGET_MS),
            ("ZAM_TEST_TOUCH_FAIL", "1"),
        ],
    );
    ok(&o);
    let log = p.log();
    assert_eq!(
        log.matches("files/changed: время очереди не обновлено")
            .count(),
        1,
        "{log:?}"
    );
    assert_eq!(
        changed_posted(&seen),
        ["src/t0.rs", "src/t1.rs", "src/t2.rs"],
        "{log}"
    );
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
/// него; вывод — тишина. Время — внутри процесса, по правилу файла.
///
/// «`Stop` эпоху не меняет» проверяется на сессии, чьи запросы эпохи шли со щедрым бюджетом
/// ([`Proj::hook`]) и потому все завершены. С бюджетом продукта проверка гонялась с сервером:
/// запрос, снятый клиентом по сроку, сервер дообрабатывает и после выхода хука (HTTP отмены не
/// несёт; строка `sessions(new_epoch): сеть` так и говорит — «сервер мог успеть её начать»), и
/// эпоха росла уже после замера (17b-http M6). Для сессий с бюджетом продукта остаётся проверка
/// без гонки — диапазон «дошло ..= дошло + снято по сроку».
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
    // лучший из трёх — полтора бюджета (срок запроса — абсолютный); худший не сверяется: одна
    // запись в файл на занятом диске изредка стоит секунду и больше
    let mut slow_times = Vec::new();
    for i in 0..3 {
        let ev = pc(&q, &format!("ps-{}", i + 1));
        let (o, d) = q.hook_at(&slow, "checkpoint", "claude", ev.as_bytes());
        empty(&o);
        slow_times.push(d);
    }
    let best = *slow_times.iter().min().unwrap();
    assert!(
        best <= max_inner("checkpoint"),
        "медленный сервер задержал хук: {slow_times:?}"
    );
    let m = EpochMisses::of(&q.log());
    let posts = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|(m, path, _)| m == "POST" && path == "/api/v1/sessions")
        .count();
    // каждый из трёх запросов ушёл и снят по сроку — или до него не дошло (промах бюджета старта
    // процесса); заглушка успевает увидеть не каждый снятый запрос (под нагрузкой — 2 из 3)
    assert!(
        m.net + m.budget >= 3 && posts <= m.net && posts <= 3,
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
    // содержимое — со щедрым бюджетом: каждый `PreCompact` — ровно +1, `Stop` эпоху не меняет
    let before = EpochMisses::of(&p.log());
    for n in 1..=2 {
        let ev: Value = serde_json::from_str(&pc(&p, "pe-2")).unwrap();
        empty(&p.hook("checkpoint", &ev));
        assert_eq!(p.session("pe-2")["seen_epoch"], n, "{}", p.log());
    }
    let st = p.ev("Stop", "pe-2", json!({"stop_hook_active": false}));
    empty(&p.hook("checkpoint", &st));
    assert_eq!(p.session("pe-2")["seen_epoch"], 2, "{}", p.log());
    assert_eq!(
        EpochMisses::of(&p.log()).since(before),
        EpochMisses::default(),
        "{}",
        p.log()
    );
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
    // содержимое (ответ 400 разобран молча) — со щедрым бюджетом: с бюджетом продукта занятая
    // машина давала вместо ответа срок запроса; заглушка отвечает сразу — время то же
    let (o, d) = p.hook_content(&url, "checkpoint", "claude", pc.to_string().as_bytes());
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
    let (o, _) = p.hook_content(&url, "checkpoint", "claude", pc.to_string().as_bytes());
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

/// Хук `kind` с щедрым бюджетом процесса и сроками `POST /code/impact` и метки контрольной
/// группы `POST /sessions {holdout}` (отладочная сборка под нагрузкой; проверка содержимого, а не
/// времени).
fn hook_slow(p: &Proj, url: &str, kind: &str, ev: &Value) -> Output {
    p.cmd(url)
        .env("ZAM_TEST_HOOK_BUDGET_MS", ROOMY_BUDGET_MS)
        .env("ZAM_TEST_HOOK_IMPACT_MS", "10000")
        .env("ZAM_TEST_HOOK_HOLDOUT_MS", "10000")
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
                    Duration::from_secs(30),
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
        // щедрый бюджет процесса; у метки — свой срок, она его не дождётся (ответ — через
        // 30 с). Срок метки здесь — 2 с вместо 100 мс продукта: на занятой машине запрос метки за
        // 100 мс не успевал дойти до заглушки, и проверять было нечего
        p.hook_env(
            &url,
            "session-start",
            "claude",
            ev.to_string().as_bytes(),
            &[
                ("ZAM_TEST_HOOK_BUDGET_MS", ROOMY_BUDGET_MS),
                ("ZAM_TEST_HOOK_HOLDOUT_MS", "2000"),
            ],
        )
        .0
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
        .env("ZAM_TEST_SNAPSHOT_PUT_MS", "10000")
        .args(["hook", kind, "--agent", "claude"])
        .write_stdin(ev.to_string().into_bytes())
        .output()
        .unwrap()
}

/// Подмена сна потока снимка (`stall-ms`, `decide-stall-ms`, `write-stall-ms`, `put-stall-ms`),
/// мс, в тестах «хук не ждёт поток»: на порядок больше порогов времени хука. Усыплённый поток
/// обрывается выходом процесса, поэтому длина сна ничего не стоит; короткий сон (2–3 с) под
/// нагрузкой кончался раньше, чем выходил задержавшийся процесс хука, — поток просыпался и
/// законно доделывал работу (удалял очередь после удавшегося `PUT`, дописывал `snapst-*`), а
/// тест падал на артефакте.
const STALL_MS: &str = "30000";

/// Время изменения файла — `secs` секунд назад (журнал «давно не менялся»).
fn age_file(path: &std::path::Path, secs: u64) {
    let t = std::time::SystemTime::now() - Duration::from_secs(secs);
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(t)
        .unwrap();
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
/// `hooks.snapshot_ms` (поток сборки «засыпает» на 30 с через точку подмены `<журнал>.stall-ms`);
/// опоздавший снимок не кладётся ни в очередь, ни на сервер. Бюджет хука — щедрый (напоминание —
/// содержимое: с бюджетом продукта занятая машина превращала его в тишину), ожидание потока
/// ограничено своим пределом (срок снимка + срок `PUT` + запас, 135 мс). Порог «хук не ждал
/// поток» — 3 с: бюджет хука здесь 10 с ([`Proj::hook_content`]), и хук, ждущий поток до конца
/// бюджета, вышел бы как раз на 10 с — общий порог [`LATE_THREAD_MAX`] этого бы не отличил.
/// Бюджет `checkpoint` — у `hooks_with_server_within_margin`.
#[test]
fn snapshot_deadline_never_delays_stop() {
    let _g = serial();
    let (url, seen) = snap_stub(204, "");
    let p = Proj::new("snap-deadline", &url, "zam_x");
    let t = transcript_path(&p, "sd-1");
    write_lines(&t, &[cl_user("sd-1", "Запрос перед остановкой")]);
    std::fs::write(format!("{}.stall-ms", t.display()), STALL_MS).unwrap();
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
    let (o, d) = p.hook_content(&url, "checkpoint", "claude", ev.to_string().as_bytes());
    let v = hook_json(&o);
    assert_eq!(v["decision"], "block", "{v}: {}", p.log());
    assert!(
        d <= Duration::from_secs(3),
        "снимок задержал Stop: {d:?}: {}",
        p.log()
    );
    assert!(puts(&seen).is_empty(), "{:?}", puts(&seen));
    assert!(snap_file(&p, "snapq", "sd-1").is_none());
    // попытка записана (`at`): следующий `Stop` — не раньше чем через 60 с
    let st = snap_file(&p, "snapst", "sd-1").expect("попытка в snapst");
    // повод и `ok_at` — только у удавшегося снимка (повторное ревью N2)
    assert_eq!(
        (&st["trigger"], &st["ok_at"]),
        (&json!(""), &json!(0)),
        "{st}"
    );
    assert!(st["at"].as_u64().unwrap() > 0);
    // через 60 с — снимок сразу: удавшегося ещё не было (размер в попытке 0, рост не нужен)
    std::fs::remove_file(format!("{}.stall-ms", t.display())).unwrap();
    age_snapst(&p, "sd-1", 61_000);
    let ev = snap_ev(&p, "Stop", "sd-1", &t, json!({"stop_hook_active": false}));
    let _ = hook_json(&hook_snap(&p, "checkpoint", &ev));
    assert_eq!(puts(&seen).len(), 1, "{}", p.log());
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
    // сменился журнал (новый файл сессии) — снимок без роста и без запроса
    age_snapst(&p, s, 61_000);
    let t2 = transcript_path(&p, "th-1-b");
    write_lines(&t2, &[cl_user(s, "запрос C")]);
    let stop2 = snap_ev(&p, "Stop", s, &t2, json!({"stop_hook_active": false}));
    empty(&hook_snap(&p, "checkpoint", &stop2));
    let got = puts(&seen);
    assert_eq!(got.len(), 4, "смена журнала: {}", p.log());
    assert_eq!(prompts_of(&got[3].1), ["запрос C"]);
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
    // бюджет — лучшим из повторов (повтор события снимок лишь заменяет): полтора бюджета
    let (o, d) = p.hook_best(&url, "checkpoint", "claude", pc.to_string().as_bytes());
    empty(&o);
    assert!(d <= max_inner("checkpoint"), "PostCompact: {d:?}");
    let end = snap_ev(&p, "SessionEnd", s, &t, json!({"reason": "exit"}));
    let (o, d) = p.hook_best(&url, "checkpoint", "claude", end.to_string().as_bytes());
    empty(&o);
    assert!(d <= max_inner("checkpoint"), "SessionEnd: {d:?}");
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
/// (`hooks.session_snapshot = false`); включённый снимок сервер (17c) принимает: очередь пуста,
/// `GET …/snapshot` отдаёт его. Сессии на сервере нет — 404 `session-not-found`: снимок остаётся
/// в очереди, счётчик попыток старого сервера не растёт, причина — в `hooks.log`; сессия
/// появилась — снимок уходит следующим хуком.
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
        assert!(snap_file(&p, "snapq", s).is_none(), "{s}: {}", p.log());
        let mut r = http()
            .get(&format!("{url}/api/v1/sessions/{s}/snapshot"))
            .header("Authorization", format!("Bearer {tok}"))
            .call()
            .unwrap();
        assert_eq!(r.status() == 200, on, "{s}: {} / {}", r.status(), p.log());
        if on {
            let v: Value = serde_json::from_str(&r.body_mut().read_to_string().unwrap()).unwrap();
            assert_eq!(v["trigger"], "session_end", "{v}");
            assert_eq!(v["agent"], "claude", "{v}");
            assert_eq!(prompts_of(&v), ["последний запрос"], "{v}");
        }
    }
    // сессии нет на сервере — `PATCH ended` молча; без журнала снимка нет
    let t = transcript_path(&p, "se-none");
    let end = snap_ev(&p, "SessionEnd", "se-none", &t, json!({"reason": "exit"}));
    empty(&hook_snap(&p, "checkpoint", &end));
    assert!(!p.log().contains("sessions(ended)"), "{}", p.log());
    assert!(snap_file(&p, "snapq", "se-none").is_none(), "{}", p.log());
    // сессии нет, журнал есть — сервер отвечает 404 `session-not-found`: снимок в очереди
    project_hooks(&p, "session_snapshot = true");
    let s = "se-unknown";
    let t = transcript_path(&p, s);
    write_lines(&t, &[cl_user(s, "запрос незарегистрированной сессии")]);
    let end = snap_ev(&p, "SessionEnd", s, &t, json!({"reason": "exit"}));
    empty(&hook_snap(&p, "checkpoint", &end));
    // больше попыток, чем даётся старому серверу: счётчик не растёт, снимок не бросается
    for _ in 0..4 {
        empty(&hook_snap(&p, "checkpoint", &end));
    }
    let q = snap_file(&p, "snapq", s).unwrap_or_else(|| panic!("нет очереди: {}", p.log()));
    assert_eq!(q["old_server"], 0, "{q}");
    let log = p.log();
    assert!(
        log.matches("HTTP 404 /problems/session-not-found").count() >= 4,
        "{log}"
    );
    assert!(!log.contains("старый сервер"), "{log}");
    // сессия появилась — очередь уходит следующим хуком
    reg(s);
    empty(&hook_snap(&p, "checkpoint", &end));
    assert!(snap_file(&p, "snapq", s).is_none(), "{}", p.log());
    let r = http()
        .get(&format!("{url}/api/v1/sessions/{s}/snapshot"))
        .header("Authorization", format!("Bearer {tok}"))
        .call()
        .unwrap();
    assert_eq!(r.status(), 200, "{}", p.log());
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
    // содержимое: срок `PUT` (60 мс) — тоже щедрый, иначе занятая машина оставляет очереди
    let o = q
        .cmd(&url3)
        .env("ZAM_TEST_HOOK_BUDGET_MS", "10000")
        .env("ZAM_TEST_SNAPSHOT_MS", "5000")
        .env("ZAM_TEST_SNAPSHOT_PUT_MS", "10000")
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
    // быстрый перезапуск: журнал менялся только что — всё равно снимается (давность не нужна)
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
    age_file(&t, 11 * 60);
    context(
        &hook_snap(&p, "session-start", &start_of("cu-d")),
        "SessionStart",
    );
    assert_eq!(puts(&seen).len(), n, "{}", p.log());
    // `resume` закрытой сессии снимает `ended`: её следующий обрыв снова догоняется
    let resume = snap_ev(&p, "SessionStart", a, &t, json!({"source": "resume"}));
    context(&hook_snap(&p, "session-start", &resume), "SessionStart");
    assert_eq!(snap_file(&p, "snapst", a).unwrap()["ended"], false);
    append_lines(&t, &[cl_user(a, "запрос после resume")]);
    age_file(&t, 11 * 60);
    context(
        &hook_snap(&p, "session-start", &start_of("cu-e")),
        "SessionStart",
    );
    let got = puts(&seen);
    assert_eq!(got.len(), n + 1, "{}", p.log());
    assert_eq!(got[n].1["trigger"], "catch_up");
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
/// `<журнал>.stall-ms` задержала бы хук на 4 с при сроке 5 с); `SessionEnd` ставит `ended`.
#[test]
fn switch_off_never_opens_transcript() {
    let _g = serial();
    let (url, seen) = snap_stub(204, "");
    let p = Proj::new("snap-off", &url, "zam_x");
    let s = "off-1";
    let t = transcript_path(&p, s);
    write_lines(&t, &[cl_user(s, "запрос")]);
    // контроль: при включённом снимке подмена задерживает хук. Здесь поток ждут (срок снимка у
    // `hook_snap` — 5 с), поэтому сон не 30 с, а наибольший под этим сроком: порог «журнал не
    // открывался» ниже — 3,9 с, запуск процесса под нагрузкой в него укладывается
    std::fs::write(format!("{}.stall-ms", t.display()), "4000").unwrap();
    let pc = snap_ev(&p, "PreCompact", "off-on", &t, json!({"trigger": "auto"}));
    let t0 = Instant::now();
    empty(&hook_snap(&p, "checkpoint", &pc));
    assert!(
        t0.elapsed() >= Duration::from_millis(3900),
        "подмена не сработала: {:?}",
        t0.elapsed()
    );
    // её журнал вырос — при включённом снимке старт сделал бы догоняющий снимок
    append_lines(&t, &[cl_user("off-on", "ещё запрос")]);
    age_file(&t, 11 * 60);
    project_hooks(&p, "session_snapshot = false");
    let n = puts(&seen).len();
    let quick = |o: Output, t0: Instant, what: &str| {
        assert!(
            t0.elapsed() < Duration::from_millis(3900),
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
    // ответ сервера с куском снимка в `detail` — в `hooks.log` только код и `type`
    let detail: &'static str = Box::leak(
        format!(r#"{{"type":"/problems/invalid","status":422,"detail":"плохое поле: {marker}"}}"#)
            .into_boxed_str(),
    );
    for (put, body) in [(404u16, r#"{"status":404}"#), (204, ""), (422, detail)] {
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

/// Ревью 17b, I1: `PUT` снимка до `/recall` — со своим сроком: медленный сервер (ответ на `PUT`
/// через 1 с) не съедает бюджет `/recall`, дайджест приходит, снимок остаётся в очереди.
#[test]
fn slow_snapshot_put_keeps_digest() {
    let _g = serial();
    // моменты приёма запросов заглушкой: путь и время
    let times: std::sync::Arc<std::sync::Mutex<Vec<(String, Instant)>>> = Default::default();
    let t2 = times.clone();
    let (url, _seen) = common::stub_body(std::sync::Arc::new(
        move |m: &str, path: &str, _: usize, _: &Value| {
            t2.lock()
                .unwrap()
                .push((format!("{m} {path}"), Instant::now()));
            if m == "PUT" && path.ends_with("/snapshot") {
                (503, r#"{"status":503}"#.to_string(), Duration::from_secs(1))
            } else if path == "/api/v1/recall" {
                (200, r#"{"text":"D"}"#.to_string(), Duration::ZERO)
            } else {
                (200, "{}".to_string(), Duration::ZERO)
            }
        },
    ));
    let p = Proj::new("snap-slow-put", &url, "zam_x");
    let s = "sp-1";
    let t = transcript_path(&p, s);
    write_lines(&t, &[cl_user(s, "запрос")]);
    // `PostCompact` снял (сервер ответил 503 — снимок в очереди)
    let pc = snap_ev(
        &p,
        "PostCompact",
        s,
        &t,
        json!({"trigger": "auto", "compact_summary": "Конспект"}),
    );
    empty(&hook_snap(&p, "checkpoint", &pc));
    assert!(snap_file(&p, "snapq", s).is_some(), "{}", p.log());
    // старт после сжатия — очередь уходит до `/recall`, но не дольше своего срока
    let ss = snap_ev(&p, "SessionStart", s, &t, json!({"source": "compact"}));
    let warm = snap_ev(
        &p,
        "SessionStart",
        "sp-warm",
        &t,
        json!({"source": "resume"}),
    );
    let _ = p.hook_at(&url, "session-start", "claude", warm.to_string().as_bytes());
    // бюджет по умолчанию: дайджест приходит в пределах хука — хотя бы в одном из трёх вызовов
    // (повтор очередь снимка не меняет: сервер отвечает 503; на занятой машине отладочная сборка
    // не всякий раз доходит до `/recall` за 270 мс)
    let mut tries = Vec::new();
    for _ in 0..3 {
        let (o, d) = p.hook_at(&url, "session-start", "claude", ss.to_string().as_bytes());
        let c = serde_json::from_str::<Value>(out(&o).trim())
            .ok()
            .and_then(|v| {
                v["hookSpecificOutput"]["additionalContext"]
                    .as_str()
                    .map(str::to_string)
            })
            .unwrap_or_default();
        let good = c.starts_with('D') && d <= max_for("session-start");
        tries.push((c, d));
        if good {
            break;
        }
    }
    let (c, d) = tries.last().unwrap();
    assert!(
        c.starts_with('D') && *d <= max_for("session-start"),
        "{tries:?}: {}",
        p.log()
    );
    // щедрый бюджет хука (3 с): `PUT` держит `/recall` только свой срок (60 мс,
    // `SNAPSHOT_PUT_MS`), а не до ответа сервера (1 с) — порог посередине, разрыв под нагрузкой
    // широкий
    const PUT_CAP_MS: u64 = 60;
    const FAIL_GAP_MS: u64 = 1000;
    let n0 = times.lock().unwrap().len();
    let o = p
        .cmd(&url)
        .env("ZAM_TEST_HOOK_BUDGET_MS", "3000")
        .args(["hook", "session-start", "--agent", "claude"])
        .write_stdin(ss.to_string().into_bytes())
        .output()
        .unwrap();
    context(&o, "SessionStart");
    let tm = times.lock().unwrap()[n0..].to_vec();
    let at = |key: &str| tm.iter().find(|(k, _)| k == key).map(|(_, t)| *t);
    let put = at(&format!("PUT /api/v1/sessions/{s}/snapshot")).expect("PUT");
    let recall = at("POST /api/v1/recall").expect("recall");
    let gap = recall.saturating_duration_since(put);
    assert!(
        gap < Duration::from_millis((PUT_CAP_MS + FAIL_GAP_MS) / 2),
        "PUT держал /recall {gap:?}"
    );
    assert!(p.log().contains("срок PUT 60 мс"), "{}", p.log());
    assert!(
        snap_file(&p, "snapq", s).is_some(),
        "снимок остался в очереди"
    );
}

/// Ревью 17b, Minor 2: ответ 400/413/422 — снимок отброшен (очередь удалена, строка в
/// `hooks.log`), следующий снимок уходит.
#[test]
fn rejected_snapshot_dropped_next_sent() {
    let _g = serial();
    let (url, seen) = common::stub_body(std::sync::Arc::new(
        |m: &str, path: &str, n: usize, _: &Value| {
            if m == "PUT" && path.ends_with("/snapshot") {
                if n == 0 {
                    (413, r#"{"status":413}"#.to_string(), Duration::ZERO)
                } else {
                    (204, String::new(), Duration::ZERO)
                }
            } else {
                (200, "{}".to_string(), Duration::ZERO)
            }
        },
    ));
    let p = Proj::new("snap-rejected", &url, "zam_x");
    let s = "rj-1";
    let t = transcript_path(&p, s);
    write_lines(&t, &[cl_user(s, "запрос 1")]);
    let pc = snap_ev(&p, "PreCompact", s, &t, json!({"trigger": "auto"}));
    empty(&hook_snap(&p, "checkpoint", &pc));
    assert!(snap_file(&p, "snapq", s).is_none(), "{}", p.log());
    assert!(p.log().contains("отброшен: HTTP 413"), "{}", p.log());
    append_lines(&t, &[cl_user(s, "запрос 2")]);
    empty(&hook_snap(&p, "checkpoint", &pc));
    let got = puts(&seen);
    assert_eq!(got.len(), 2, "{}", p.log());
    assert_eq!(prompts_of(&got[1].1), ["запрос 1", "запрос 2"]);
    assert!(snap_file(&p, "snapq", s).is_none());
}

/// Ревью 17b, Minor 3: очередь уходит следующим хуком и без нового снимка (`Stop` раньше 60 с).
#[test]
fn queued_snapshot_sent_without_new_one() {
    let _g = serial();
    let (down, _) = snap_stub(503, r#"{"status":503}"#);
    let p = Proj::new("snap-pending", &down, "zam_x");
    let s = "pd-1";
    let t = transcript_path(&p, s);
    write_lines(&t, &[cl_user(s, "запрос")]);
    let stop = snap_ev(&p, "Stop", s, &t, json!({"stop_hook_active": false}));
    empty(&hook_snap(&p, "checkpoint", &stop));
    assert!(snap_file(&p, "snapq", s).is_some(), "{}", p.log());
    let (up, seen) = snap_stub(204, "");
    let at = |url: &str, ev: &Value| {
        let o = p
            .cmd(url)
            .env("ZAM_TEST_HOOK_BUDGET_MS", "10000")
            .env("ZAM_TEST_SNAPSHOT_MS", "5000")
            .env("ZAM_TEST_SNAPSHOT_PUT_MS", "10000")
            .args(["hook", "checkpoint", "--agent", "claude"])
            .write_stdin(ev.to_string().into_bytes())
            .output()
            .unwrap();
        empty(&o);
    };
    // `Stop` раньше 60 с — задания снимка нет, очередь уходит сама
    at(&up, &stop);
    let got = puts(&seen);
    assert_eq!(got.len(), 1, "{}", p.log());
    assert_eq!(got[0].1["trigger"], "stop");
    assert!(snap_file(&p, "snapq", s).is_none());
    // снова в очереди; `Stop` через 60 с без роста — снимок не нужен, очередь уходит
    let pc = snap_ev(&p, "PreCompact", s, &t, json!({"trigger": "auto"}));
    at(&down, &pc);
    assert!(snap_file(&p, "snapq", s).is_some(), "{}", p.log());
    age_snapst(&p, s, 61_000);
    at(&up, &stop);
    let got = puts(&seen);
    assert_eq!(got.len(), 2, "{}", p.log());
    assert_eq!(
        got[1].1["trigger"], "pre_compact",
        "новый снимок не снимался"
    );
    assert!(snap_file(&p, "snapq", s).is_none());
}

/// Ревью 17b, Minor 9: `PostCompact` без `transcript_path`, но с конспектом — снимок из одного
/// конспекта.
#[test]
fn post_compact_without_transcript_uses_summary() {
    let _g = serial();
    let (url, seen) = snap_stub(204, "");
    let p = Proj::new("snap-pc-no-t", &url, "zam_x");
    let mut pc = p.ev(
        "PostCompact",
        "pn-1",
        json!({"trigger": "auto", "compact_summary": "Следующий шаг: тесты"}),
    );
    pc.as_object_mut().unwrap().remove("transcript_path");
    empty(&hook_snap(&p, "checkpoint", &pc));
    let got = puts(&seen);
    assert_eq!(got.len(), 1, "{}", p.log());
    assert_eq!(got[0].1["data"]["compact_summary"], "Следующий шаг: тесты");
    assert_eq!(got[0].1["data"]["prompts"], json!([]));
}

/// Ревью 17b, Minor 10: правка очереди по ответу — только под замком (чужой замок — очередь не
/// трогается, снимок уйдёт ещё раз); очередь больше предела кэша не читается.
#[test]
fn queue_lock_and_size_limit() {
    let _g = serial();
    let (url, seen) = snap_stub(204, "");
    let p = Proj::new("snap-lock", &url, "zam_x");
    let s = "lk-1";
    let t = transcript_path(&p, s);
    write_lines(&t, &[cl_user(s, "запрос")]);
    let (down, _) = snap_stub(503, r#"{"status":503}"#);
    let pc = snap_ev(&p, "PreCompact", s, &t, json!({"trigger": "auto"}));
    let o = p
        .cmd(&down)
        .env("ZAM_TEST_HOOK_BUDGET_MS", "10000")
        .env("ZAM_TEST_SNAPSHOT_MS", "5000")
        .args(["hook", "checkpoint", "--agent", "claude"])
        .write_stdin(pc.to_string().into_bytes())
        .output()
        .unwrap();
    empty(&o);
    let q = p.session_file("snapq", s, ".json").expect("очередь");
    let name = q.file_name().unwrap().to_str().unwrap().to_string();
    let lock = q.with_file_name(format!(".{name}.lock"));
    std::fs::write(&lock, "чужой").unwrap();
    let stop = snap_ev(&p, "Stop", s, &t, json!({"stop_hook_active": false}));
    empty(&hook_snap(&p, "checkpoint", &stop));
    assert_eq!(puts(&seen).len(), 1, "{}", p.log());
    assert!(q.is_file(), "очередь удалена без замка");
    assert!(
        p.log()
            .contains("отправлен, очередь оставлена (замок занят)"),
        "{}",
        p.log()
    );
    std::fs::remove_file(&lock).unwrap();
    // очередь больше 16 МиБ — не читается, хук молчит
    let big = format!(
        r#"{{"session":"{s}","old_server":0,"body":{{"pad":"{}"}}}}"#,
        "x".repeat(17 * 1024 * 1024)
    );
    std::fs::write(&q, big).unwrap();
    empty(&hook_snap(&p, "checkpoint", &stop));
    assert_eq!(puts(&seen).len(), 1, "{}", p.log());
}

/// Ревью 17b, Minor 11: без адреса сервера `session-start` пишет одну строку «не заданы адрес
/// сервера или токен» (снимок — только в очередь).
#[test]
fn no_server_single_log_line() {
    let _g = serial();
    let p = Proj::new("snap-no-server", DEAD, "zam_x");
    let s = "ns-1";
    let t = transcript_path(&p, s);
    write_lines(&t, &[cl_user(s, "запрос")]);
    let ss = snap_ev(&p, "SessionStart", s, &t, json!({"source": "compact"}));
    let o = p
        .cmd(DEAD)
        .env_remove("ZAM_URL")
        .env("ZAM_TEST_HOOK_BUDGET_MS", "10000")
        .env("ZAM_TEST_SNAPSHOT_MS", "5000")
        .args(["hook", "session-start", "--agent", "claude"])
        .write_stdin(ss.to_string().into_bytes())
        .output()
        .unwrap();
    ok(&o);
    let log = p.log();
    assert_eq!(
        log.matches("не заданы адрес сервера или токен").count(),
        1,
        "{log}"
    );
    assert!(snap_file(&p, "snapq", s).is_some(), "{log}");
}

/// Ревью 17b, Minor 12: опоздавший догоняющий снимок (подмена `stall-ms` на журнале другой
/// сессии) не задерживает старт.
#[test]
fn late_catch_up_does_not_delay_start() {
    let _g = serial();
    let (url, seen) = snap_stub(204, "");
    let p = Proj::new("snap-late-catch", &url, "zam_x");
    let a = "lc-a";
    let t = transcript_path(&p, a);
    write_lines(&t, &[cl_user(a, "запрос 1")]);
    let stop = snap_ev(&p, "Stop", a, &t, json!({"stop_hook_active": false}));
    empty(&hook_snap(&p, "checkpoint", &stop));
    assert_eq!(puts(&seen).len(), 1, "{}", p.log());
    append_lines(&t, &[cl_user(a, "запрос 2")]);
    age_file(&t, 11 * 60);
    std::fs::write(format!("{}.stall-ms", t.display()), STALL_MS).unwrap();
    let tb = transcript_path(&p, "lc-b");
    write_lines(&tb, &[]);
    let warm = snap_ev(
        &p,
        "SessionStart",
        "lc-warm",
        &tb,
        json!({"source": "resume"}),
    );
    let _ = p.hook_at(&url, "session-start", "claude", warm.to_string().as_bytes());
    let ss = snap_ev(
        &p,
        "SessionStart",
        "lc-b",
        &tb,
        json!({"source": "startup"}),
    );
    let (o, d) = p.hook_at(&url, "session-start", "claude", ss.to_string().as_bytes());
    ok(&o);
    assert!(
        d <= max_for("session-start"),
        "догоняющий задержал старт: {d:?}"
    );
    assert_eq!(puts(&seen).len(), 1, "опоздавший снимок не уходит");
}

/// Повторное ревью 17b, N1: быстрый перезапуск — оборванная сессия A снимается сразу при старте
/// B; после снимка B (её `snapst-*` новее) старт C не теряет и не портит A (снимок A не повторяется,
/// её `snapst-*` прежний).
#[test]
fn catch_up_fast_restart_keeps_abandoned() {
    let _g = serial();
    let (url, seen) = snap_stub(204, "");
    let p = Proj::new("snap-fast-restart", &url, "zam_x");
    let (a, b) = ("fr-a", "fr-b");
    let ta = transcript_path(&p, a);
    write_lines(&ta, &[cl_user(a, "запрос A1")]);
    let stop_a = snap_ev(&p, "Stop", a, &ta, json!({"stop_hook_active": false}));
    empty(&hook_snap(&p, "checkpoint", &stop_a));
    // A оборвалась после ещё одного запроса, B стартует сразу
    append_lines(&ta, &[cl_user(a, "запрос A2 перед обрывом")]);
    let tb = transcript_path(&p, b);
    write_lines(&tb, &[cl_user(b, "запрос B1")]);
    let ss_b = snap_ev(&p, "SessionStart", b, &tb, json!({"source": "startup"}));
    context(&hook_snap(&p, "session-start", &ss_b), "SessionStart");
    let got = puts(&seen);
    assert_eq!(got.len(), 2, "{}", p.log());
    assert_eq!(got[1].0, format!("/api/v1/sessions/{a}/snapshot"));
    assert_eq!(got[1].1["trigger"], "catch_up");
    assert_eq!(
        prompts_of(&got[1].1),
        ["запрос A1", "запрос A2 перед обрывом"]
    );
    let st_a = snap_file(&p, "snapst", a).unwrap();
    // первый `Stop` B — её `snapst-*` становится самым новым
    let stop_b = snap_ev(&p, "Stop", b, &tb, json!({"stop_hook_active": false}));
    empty(&hook_snap(&p, "checkpoint", &stop_b));
    assert_eq!(puts(&seen).len(), 3, "{}", p.log());
    let tc = transcript_path(&p, "fr-c");
    write_lines(&tc, &[]);
    let ss_c = snap_ev(
        &p,
        "SessionStart",
        "fr-c",
        &tc,
        json!({"source": "startup"}),
    );
    context(&hook_snap(&p, "session-start", &ss_c), "SessionStart");
    let got = puts(&seen);
    assert_eq!(got.len(), 3, "лишний снимок: {got:?}");
    assert_eq!(
        snap_file(&p, "snapst", a).unwrap(),
        st_a,
        "snapst A испорчен"
    );
}

/// Повторное ревью 17b, N2: опоздавший `PostCompact` (подмена `stall-ms`) не считается
/// пришедшим — `SessionStart source=compact` снимает журнал сам (`session_start_compact`).
#[test]
fn late_post_compact_not_counted() {
    let _g = serial();
    let (url, seen) = snap_stub(204, "");
    let p = Proj::new("snap-late-pc", &url, "zam_x");
    let s = "lp-1";
    let t = transcript_path(&p, s);
    write_lines(&t, &[cl_user(s, "запрос перед сжатием")]);
    // удавшийся снимок до сжатия (`ok_at` свежий): опоздавшая попытка не должна менять повод
    let pre = snap_ev(&p, "PreCompact", s, &t, json!({"trigger": "auto"}));
    empty(&hook_snap(&p, "checkpoint", &pre));
    assert_eq!(puts(&seen).len(), 1, "{}", p.log());
    let stall = format!("{}.stall-ms", t.display());
    std::fs::write(&stall, STALL_MS).unwrap();
    let pc = snap_ev(
        &p,
        "PostCompact",
        s,
        &t,
        json!({"trigger": "auto", "compact_summary": "Конспект"}),
    );
    let (o, _) = p.hook_at(&url, "checkpoint", "claude", pc.to_string().as_bytes());
    empty(&o);
    assert_eq!(puts(&seen).len(), 1, "опоздавший снимок ушёл");
    let st = snap_file(&p, "snapst", s).expect("попытка в snapst");
    assert_eq!(st["trigger"], "pre_compact", "{st}");
    std::fs::remove_file(&stall).unwrap();
    let ss = snap_ev(&p, "SessionStart", s, &t, json!({"source": "compact"}));
    context(&hook_snap(&p, "session-start", &ss), "SessionStart");
    let got = puts(&seen);
    assert_eq!(got.len(), 2, "{}", p.log());
    assert_eq!(got[1].1["trigger"], "session_start_compact");
}

/// `PreCompact` с бюджетом хука 1,5 с и своим сроком `PUT` 10 с против заглушки, отвечающей на
/// `PUT` кодом 204 через `put_delay`: при задержке больше бюджета фактический срок `PUT` —
/// остаток бюджета. `put_stall` — подмена `<кэш>/snapshot.put-stall-ms` ([`STALL_MS`]): поток
/// снимка засыпает между концом `PUT` и работой с очередью. Возвращает `hooks.log`, время хука и
/// наличие очереди снимка.
fn put_limit_case(name: &str, put_delay: Duration, put_stall: bool) -> (String, Duration, bool) {
    let (url, _seen) = common::stub_body(std::sync::Arc::new(
        move |m: &str, path: &str, _: usize, _: &Value| {
            if m == "PUT" && path.ends_with("/snapshot") {
                (204, String::new(), put_delay)
            } else {
                (200, "{}".to_string(), Duration::ZERO)
            }
        },
    ));
    let p = Proj::new(name, &url, "zam_x");
    let s = "pl-1";
    let t = transcript_path(&p, s);
    write_lines(&t, &[cl_user(s, "запрос")]);
    if put_stall {
        let cache = p.repo.join(".zeroaimemory/cache");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("snapshot.put-stall-ms"), STALL_MS).unwrap();
    }
    let pc = snap_ev(&p, "PreCompact", s, &t, json!({"trigger": "auto"}));
    // свой срок `PUT` больше остатка бюджета: фактический срок — остаток (< 1500 мс)
    let mut c = p.cmd(&url);
    c.env("ZAM_TEST_HOOK_BUDGET_MS", "1500")
        .env("ZAM_TEST_SNAPSHOT_MS", "5000")
        .env("ZAM_TEST_SNAPSHOT_PUT_MS", "10000")
        .args(["hook", "checkpoint", "--agent", "claude"])
        .write_stdin(pc.to_string().into_bytes());
    let t0 = Instant::now();
    let o = c.output().unwrap();
    let d = t0.elapsed();
    empty(&o);
    (p.log(), d, snap_file(&p, "snapq", s).is_some())
}

/// Срок из единственной строки «срок PUT N мс» журнала хуков: фактический — больше нуля и
/// меньше бюджета хука (1500 мс).
fn put_limit_logged(log: &str) -> u64 {
    assert_eq!(
        log.matches("срок PUT ").count(),
        1,
        "строка «срок PUT» — ровно одна: {log:?}"
    );
    let n: u64 = log
        .split("срок PUT ")
        .nth(1)
        .and_then(|r| r.split_whitespace().next())
        .and_then(|x| x.parse().ok())
        .unwrap_or_else(|| panic!("срок PUT не число: {log}"));
    assert!(n > 0 && n < 1500, "срок {n} мс — не фактический: {log}");
    n
}

/// Предел времени хука в тестах «хук не ждёт усыплённый поток» (сон — [`STALL_MS`], 30 с): свой
/// предел ожидания у хука — доли секунды, остановки диска и запуск отладочного процесса под
/// нагрузкой добавляют до 3–4 с, а хук, ждущий поток, шёл бы не меньше 15 с (сроки снимка и
/// `PUT` в этих тестах) или до конца сна. Годится только там, где бюджет хука заметно меньше
/// порога (1–4 с) или заметно больше (30 с): при бюджете 10 с хук, досидевший бюджет, уложился
/// бы в порог — у такого теста порог свой (`snapshot_deadline_never_delays_stop`).
const LATE_THREAD_MAX: Duration = Duration::from_secs(10);

/// Третье ревью 17b, Minor 1: срок `PUT` — фактический срок клиента (меньшее из своего срока и
/// остатка бюджета хука); причина «срок PUT N мс» — с ним, а не с константой.
#[test]
fn put_timeout_reason_uses_actual_limit() {
    let _g = serial();
    let (log, _, queued) = put_limit_case("snap-put-limit", Duration::from_secs(3), false);
    put_limit_logged(&log);
    assert!(queued, "{log}");
}

/// Хук `checkpoint` (`PreCompact`, бюджет 1,5 с), у которого поток снимка пишет строку итога
/// под замком `stall_ms` мс (подмена `log-stall-ms`), начав запись незадолго до предела ожидания
/// хука: заглушка отвечает на `PUT` 500 через 1,3 с. Возвращает `hooks.log`, время хука внутри
/// процесса и наличие очереди снимка (и сам проект — для следующих хуков).
fn slow_log_case(name: &str, stall_ms: &str) -> (Proj, String, Duration, bool) {
    let (url, _seen) = common::stub_body(std::sync::Arc::new(
        move |m: &str, path: &str, _: usize, _: &Value| {
            if m == "PUT" && path.ends_with("/snapshot") {
                (500, String::new(), Duration::from_millis(1300))
            } else {
                (200, "{}".to_string(), Duration::ZERO)
            }
        },
    ));
    let p = Proj::new(name, &url, "zam_x");
    let s = "sl-1";
    let t = transcript_path(&p, s);
    write_lines(&t, &[cl_user(s, "запрос")]);
    let cache = p.repo.join(".zeroaimemory/cache");
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(cache.join("snapshot.log-stall-ms"), stall_ms).unwrap();
    let pc = snap_ev(&p, "PreCompact", s, &t, json!({"trigger": "auto"}));
    let (o, times) = p.hook_env(
        &url,
        "checkpoint",
        "claude",
        pc.to_string().as_bytes(),
        &[
            ("ZAM_TEST_HOOK_BUDGET_MS", "1500"),
            ("ZAM_TEST_SNAPSHOT_MS", "5000"),
            ("ZAM_TEST_SNAPSHOT_PUT_MS", "10000"),
        ],
    );
    empty(&o);
    let (log, queued) = (p.log(), snap_file(&p, "snapq", s).is_some());
    (p, log, times.inner, queued)
}

/// Предел времени хука в [`slow_log_case`]: бюджет 1,5 с + предел ожидания записи строки потоком
/// снимка (`LOG_WAIT_MAX`, 250 мс) + запас отладочной сборки на файлы.
const SLOW_LOG_MAX: Duration = Duration::from_millis(1500 + 250 + 250);

/// Задача 17d, пункт h (слияние 05f41fc: `put_timeout_reason_uses_actual_limit` упал с пустым
/// `hooks.log`): поток снимка пишет строку итога под замком, а запись идёт дольше хвоста ожидания
/// хука (на занятом HDD создание `hooks.log` стоит десятки мс при хвосте 10 мс). Запись длится
/// 200 мс и застаёт предел ожидания хука: хук ждёт её конца — раньше он выходил, процесс обрывал
/// поток, и неотправленный снимок не оставлял ни одной строки причины. Строка — ровно одна
/// (потока; либо хука, если поток до записи ещё не дошёл), снимок в очереди, а хук вышел не
/// позже бюджета и предела ожидания записи.
#[test]
fn snapshot_outcome_survives_slow_log_write() {
    let _g = serial();
    let (_p, log, d, queued) = slow_log_case("snap-slow-log", "200");
    assert_eq!(
        log.matches("HTTP 500").count(),
        1,
        "строка причины — ровно одна (хук шёл {d:?}): {log:?}"
    );
    assert_eq!(log.lines().count(), 1, "{log}");
    assert!(queued, "снимка нет в очереди: {log}");
    assert!(d <= SLOW_LOG_MAX, "хук шёл {d:?}: {log}");
}

/// Ревью 17d, I1: запись строки потоком снимка стоит (диск остановился на 5 с) — хук ждёт её не
/// дольше `LOG_WAIT_MAX` и выходит: вывод хука (`decision: block` у `Stop`) не зависит от диска,
/// агент не убивает хук по своему тайм-ауту (2 с). Строки итога в этом вызове нет, снимок в
/// очереди, и след оставляет следующий хук сессии: отправка очереди всегда пишет строку.
#[test]
fn snapshot_log_wait_is_bounded() {
    let _g = serial();
    let (p, log, d, queued) = slow_log_case("snap-stuck-log", "5000");
    assert!(
        d <= SLOW_LOG_MAX,
        "хук ждал запись строки без предела: {d:?}: {log}"
    );
    assert!(queued, "снимка нет в очереди: {log}");
    // строку мог успеть написать только сам хук (поток стоит): не больше одной
    assert!(log.lines().count() <= 1, "{log}");
    // следующий хук сессии (запись строки больше не стоит): очередь отправляется заново, и её
    // итог — строкой в журнале
    std::fs::remove_file(p.repo.join(".zeroaimemory/cache/snapshot.log-stall-ms")).unwrap();
    let t = transcript_path(&p, "sl-1");
    let stop = snap_ev(&p, "Stop", "sl-1", &t, json!({"stop_hook_active": true}));
    empty(&hook_snap(&p, "checkpoint", &stop));
    let log = p.log();
    assert!(
        log.contains("снимок из очереди, в очереди: HTTP 500"),
        "следующий хук не оставил строки об очереди: {log:?}"
    );
}

/// Задача 17d, пункт h: журнал агента есть, но не читается (открыт другим процессом без
/// совместного доступа) — снимка нет, но в `hooks.log` строка причины, а не тишина.
#[cfg(windows)]
#[test]
fn snapshot_unreadable_transcript_is_logged() {
    use std::os::windows::fs::OpenOptionsExt;
    let _g = serial();
    let (url, seen) = snap_stub(204, "");
    let p = Proj::new("snap-unreadable", &url, "zam_x");
    let s = "ur-1";
    let t = transcript_path(&p, s);
    write_lines(&t, &[cl_user(s, "запрос")]);
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&t)
        .unwrap();
    let pc = snap_ev(&p, "PreCompact", s, &t, json!({"trigger": "auto"}));
    empty(&hook_snap(&p, "checkpoint", &pc));
    drop(held);
    let log = p.log();
    assert_eq!(
        log.matches("снимок pre_compact: журнал агента не прочитан")
            .count(),
        1,
        "{log:?}"
    );
    assert!(puts(&seen).is_empty(), "{log}");
    assert!(snap_file(&p, "snapq", s).is_none());
}

/// CI ubuntu (77063ec): срок `PUT` истекает за `HTTP_RESERVE_MS` (30 мс) до конца бюджета, и
/// поток снимка может не успеть записать итог до выхода хука — строка терялась. Подмена
/// `put-stall-ms` держит поток 30 с после конца `PUT`: причина «срок PUT N мс» всё равно в
/// `hooks.log` (её пишет сам хук), ровно одна, снимок в очереди, хук не ждёт поток.
#[test]
fn put_outcome_logged_when_snapshot_thread_is_late() {
    let _g = serial();
    let slow = Duration::from_secs(3);
    // прогрев: первый запуск процесса после сборки не меряется
    let _ = put_limit_case("snap-put-late-warm", slow, true);
    let (log, d, queued) = put_limit_case("snap-put-late-log", slow, true);
    // время — первым: у задержавшегося хука прочие проверки ловили бы артефакты проснувшегося
    // потока, а не причину
    assert!(
        d <= LATE_THREAD_MAX,
        "хук ждал поток снимка: {d:?} при пределе {LATE_THREAD_MAX:?}: {log}"
    );
    put_limit_logged(&log);
    // строку пишет сам хук, на каком бы этапе он ни застал поток: отказ клиента по сроку уже
    // вернулся («…срок PUT N мс, очередь не обработана до выхода хука») или ещё нет («…срок PUT
    // N мс, итог отправки не записан до выхода хука» — CI windows-latest, 76458e3: отказ
    // вернулся позже предела ожидания). Оба исхода верны; своей строки («…, в очереди: срок PUT»)
    // усыплённый поток записать не может
    assert!(
        log.contains("до выхода хука") && !log.contains(", в очереди"),
        "строку итога написал не хук (хук шёл {d:?}): {log}"
    );
    assert!(queued, "снимка нет в очереди (хук шёл {d:?}): {log}");
}

/// Ревью 2c36319, I1: сервер ответил 204 сразу, а поток снимка застрял после `PUT` (подмена
/// `put-stall-ms` 30 с) — хук пишет «ответ получен (HTTP 204), очередь не обработана», а не
/// «срок PUT»: медленная локальная доработка не выдаётся за медленный сервер.
#[test]
fn late_snapshot_thread_after_response_is_not_put_timeout() {
    let _g = serial();
    // прогрев: первый запуск процесса после сборки не меряется
    let _ = put_limit_case("snap-put-done-warm", Duration::ZERO, true);
    let (log, d, queued) = put_limit_case("snap-put-done-late", Duration::ZERO, true);
    // время — первым: проснувшийся через 30 с поток законно удалил бы очередь после удавшегося
    // `PUT`, и проверка очереди упала бы на этом артефакте, а не на причине
    assert!(
        d <= LATE_THREAD_MAX,
        "хук ждал поток снимка: {d:?} при пределе {LATE_THREAD_MAX:?}: {log}"
    );
    assert!(
        !log.contains("срок PUT"),
        "ложная причина (хук шёл {d:?}): {log}"
    );
    assert_eq!(
        log.matches("ответ получен (HTTP 204), очередь не обработана до выхода хука")
            .count(),
        1,
        "хук шёл {d:?}: {log:?}"
    );
    // поток оборван до работы с очередью: снимок уйдёт ещё раз со следующим хуком
    assert!(queued, "снимка нет в очереди (хук шёл {d:?}): {log}");
}

/// Повторное ревью 17b (`prompt_flushes_changed_queue`): решение о снимке и снимок — вне пути
/// сетевой части `checkpoint`. Подмена `<журнал>.decide-stall-ms` усыпляет поток снимка до
/// решения на 30 с: очередь изменённых файлов всё равно уходит в бюджете хука, хук не ждёт поток.
///
/// Бюджет хука — 4 с (`ZAM_TEST_HOOK_BUDGET_MS`), пауза решения — 30 с: решение на главном пути
/// съело бы бюджет целиком (очередь не ушла бы, хук шёл бы 30 с), а занятая машина в него
/// укладывается. С бюджетом продукта (70 мс на HTTP) тест путал одно с другим: на занятом диске
/// одна операция с файлом очереди стоит до 1,5 с (замер под нагрузкой, журнал M6), и очередь не
/// уходила без всякого участия потока снимка. Бюджеты меряют тесты времени и `hook_budgets`.
#[test]
fn snapshot_decision_never_delays_changed_flush() {
    let _g = serial();
    let (url, seen) = snap_stub(204, "");
    let p = Proj::new("snap-decide-stall", &url, "zam_x");
    let s = "ds-1";
    std::fs::create_dir_all(p.repo.join("src")).unwrap();
    std::fs::write(p.repo.join("src/pool.rs"), b"v1\n").unwrap();
    let edit = post_ok(
        &p,
        s,
        "Edit",
        json!({"file_path": p.repo.join("src/pool.rs")}),
    );
    let _ = p.hook_at(&url, "post-tool", "claude", edit.to_string().as_bytes());
    assert_eq!(queue(&p, s).len(), 1);
    let t = transcript_path(&p, s);
    write_lines(&t, &[cl_user(s, "запрос")]);
    std::fs::write(format!("{}.decide-stall-ms", t.display()), STALL_MS).unwrap();
    let stop = snap_ev(&p, "Stop", s, &t, json!({"stop_hook_active": false}));
    let mut c = p.cmd(&url);
    c.env("ZAM_TEST_HOOK_BUDGET_MS", "4000")
        .args(["hook", "checkpoint", "--agent", "claude"])
        .write_stdin(stop.to_string().into_bytes());
    let t0 = Instant::now();
    let o = c.output().unwrap();
    let d = t0.elapsed();
    ok(&o);
    // хук не ждёт поток снимка: опоздавшему он отдаёт остаток бюджета (4 с) и выходит — задолго
    // до конца паузы решения (30 с); запас 4 с — запуск и выход процесса на занятой машине
    assert!(
        d <= Duration::from_millis(8000),
        "поток снимка задержал хук: {d:?}"
    );
    let sent = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|(m, path, _)| m == "POST" && path == "/api/v1/files/changed")
        .count();
    // что именно не успело — в журнале хука: «files/changed: очередь не отправлена — остаток
    // бюджета меньше 15 мс (прошло N мс)» (пропуск до запроса) или «files/changed: <ошибка сети>»
    assert!(
        sent >= 1,
        "очередь изменённых не ушла (хук шёл {d:?}): {}",
        p.log()
    );
    assert!(queue(&p, s).is_empty(), "{}", p.log());
}

/// Ревью правки `checkpoint`, Minor 1: ожидание потока снимка ограничено концом бюджета хука —
/// медленная сеть (2 с) и медленное решение (`decide-stall-ms` 30 с) не уводят выход `Stop` за
/// бюджет (1 с). Свой предел ожидания здесь — 20 с от запуска потока (срок `PUT`): хук, ждущий
/// поток по своему пределу или от конца сетевой части, шёл бы 20 с и дольше при пороге 10 с.
#[test]
fn snapshot_wait_bounded_by_hook_budget() {
    let _g = serial();
    let (url, _seen) = common::stub(
        200,
        r#"{"memory_writes":0,"claimed_tasks":[]}"#,
        Duration::from_secs(2),
    );
    let p = Proj::new("snap-wait-budget", &url, "zam_x");
    let s = "wb-1";
    empty(&p.hook("pre-tool", &write_ev(&p, s, "a.sh")));
    let t = transcript_path(&p, s);
    write_lines(&t, &[cl_user(s, "запрос")]);
    std::fs::write(format!("{}.decide-stall-ms", t.display()), STALL_MS).unwrap();
    let stop = snap_ev(&p, "Stop", s, &t, json!({"stop_hook_active": false}));
    let run = || {
        let mut c = p.cmd(&url);
        c.env("ZAM_TEST_HOOK_BUDGET_MS", "1000")
            .env("ZAM_TEST_SNAPSHOT_PUT_MS", "20000")
            .args(["hook", "checkpoint", "--agent", "claude"])
            .write_stdin(stop.to_string().into_bytes());
        let t0 = Instant::now();
        let o = c.output().unwrap();
        (o, t0.elapsed())
    };
    let _ = run();
    let (o, d) = run();
    ok(&o);
    let log = p.log();
    assert!(
        d <= LATE_THREAD_MAX,
        "поток снимка увёл хук за бюджет: {d:?} при пределе {LATE_THREAD_MAX:?}: {log}"
    );
    // ревью 2c36319, M4: поток до `PUT` не дошёл — строка с событием и временем от старта
    assert!(
        log.contains("snapshot: не готов до выхода хука (Stop, прошло "),
        "{log}"
    );
}

/// Ревью правки `checkpoint`, Minor 3: сначала очередь, потом `snapst-*` — обрыв между записями
/// (подмена `write-stall-ms`, хук выходит по своему пределу) оставляет снимок в очереди без
/// отметки «снят», а не отметку без снимка.
#[test]
fn snapshot_queue_written_before_state() {
    let _g = serial();
    let (url, _seen) = snap_stub(204, "");
    let p = Proj::new("snap-write-order", &url, "zam_x");
    let s = "wo-1";
    let t = transcript_path(&p, s);
    write_lines(&t, &[cl_user(s, "запрос")]);
    std::fs::write(format!("{}.write-stall-ms", t.display()), STALL_MS).unwrap();
    let pc = snap_ev(&p, "PreCompact", s, &t, json!({"trigger": "auto"}));
    // прогрев: первый запуск процесса после сборки не меряется
    let warm = snap_ev(&p, "Stop", "wo-warm", &t, json!({"stop_hook_active": true}));
    let _ = p.hook_at(&url, "checkpoint", "claude", warm.to_string().as_bytes());
    let mut c = p.cmd(&url);
    c.env("ZAM_TEST_HOOK_BUDGET_MS", "30000")
        .env("ZAM_TEST_SNAPSHOT_MS", "500")
        .args(["hook", "checkpoint", "--agent", "claude"])
        .write_stdin(pc.to_string().into_bytes());
    let t0 = Instant::now();
    let o = c.output().unwrap();
    let d = t0.elapsed();
    empty(&o);
    // третье ревью 599c933: хук выходит по своему пределу (срок снимка 500 мс + срок `PUT` 60 мс
    // + запас 50 мс от запуска потока), а не досиживает бюджет 30 с вслед за потоком. Время —
    // первым: проснувшийся поток дописал бы `snapst-*`, и проверка ниже упала бы на артефакте
    assert!(
        d <= LATE_THREAD_MAX,
        "хук ждал поток до конца бюджета: {d:?} при пределе {LATE_THREAD_MAX:?}: {}",
        p.log()
    );
    assert!(
        snap_file(&p, "snapq", s).is_some(),
        "снимка нет в очереди (хук шёл {d:?}): {}",
        p.log()
    );
    let st = snap_file(&p, "snapst", s);
    assert!(
        st.as_ref().is_none_or(|v| v["ok_at"] == 0),
        "отметка «снят» без очереди (хук шёл {d:?}): {st:?}"
    );
}

/// Ревью правки `checkpoint`, Minor 4: паника потока снимка (тестовая точка `<журнал>.panic`) —
/// строка «snapshot: паника» в `hooks.log`, хук отвечает как обычно.
#[test]
fn snapshot_thread_panic_logged() {
    let _g = serial();
    let (url, _seen) = snap_stub(204, "");
    let p = Proj::new("snap-panic", &url, "zam_x");
    let s = "pn-1";
    let t = transcript_path(&p, s);
    write_lines(&t, &[cl_user(s, "запрос")]);
    std::fs::write(format!("{}.panic", t.display()), "").unwrap();
    let stop = snap_ev(&p, "Stop", s, &t, json!({"stop_hook_active": false}));
    empty(&hook_snap(&p, "checkpoint", &stop));
    assert!(p.log().contains("snapshot: паника"), "{}", p.log());
}

// ---- M6 §16 п. 19: пакет субагента без `agent_id` не гасит подсказки родителя ----

/// `GET /sessions` токеном проекта: сессии с `parent_id == parent`.
fn t19_children(p: &Proj, parent: &str) -> Vec<Value> {
    let r = http()
        .get(&format!("{}/api/v1/sessions?limit=500", p.url))
        .header("Authorization", format!("Bearer {}", p.token))
        .call()
        .unwrap();
    let v: Value = serde_json::from_str(&r.into_body().read_to_string().unwrap()).unwrap();
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["parent_id"] == parent)
        .cloned()
        .collect()
}

/// Подсказки `prompt` сессии `session` к тексту `prompt` (REST, без бюджета хука): ссылки.
fn t19_prompt_refs(p: &Proj, session: &str, prompt: &str) -> Vec<String> {
    let v = api_post(
        p,
        "/recall",
        json!({"event": "prompt", "session_id": session, "prompt": prompt, "solution": G}),
    );
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["id"].as_str().map(str::to_string))
        .collect()
}

/// Приложение zero-agent: `SubagentStart` без `agent_id` — пакет `brief` идёт в своей сессии
/// субагента (с `parent_id` родителя), а не в сессии родителя: показ пакета не отмечается у
/// родителя, и его подсказка `prompt` к той же ловушке не гасится. Контроль — та же подсказка
/// в сессии без пакета.
#[test]
fn subagent_brief_does_not_mute_parent_hints() {
    let _g = serial();
    let (url, tok) = start("t19-brief-mute");
    let p = Proj::new("t19-brief-mute", &url, &tok);
    let pulsar = p.insight("Пульсар мигает по таймеру", "gotcha", "global", &[]);
    let want = format!("i:{pulsar}");
    let prompt = "почему пульсар мигает по таймеру";
    // контроль: подсказка к запросу в сессии без пакета
    assert!(
        t19_prompt_refs(&p, "t19-ctl", prompt).contains(&want),
        "контроль без подсказки"
    );
    // пакет субагенту без agent_id: ловушка в пакете
    let ev = p.ev("SubagentStart", "t19-par", json!({"agent_type": "Plan"}));
    let c = context(&p.hook("subagent-start", &ev), "SubagentStart");
    assert!(c.contains(&want), "{c}");
    // у родителя подсказка не погашена показом пакета
    assert!(
        t19_prompt_refs(&p, "t19-par", prompt).contains(&want),
        "пакет субагента погасил подсказку родителя"
    );
    // сессия субагента зарегистрирована с parent_id родителя
    let kids = t19_children(&p, "t19-par");
    assert_eq!(kids.len(), 1, "{kids:?}");
    assert_eq!(kids[0]["agent_type"], "Plan", "{kids:?}");
    assert_ne!(kids[0]["id"], "t19-par");
    // второй субагент без agent_id — своя сессия
    let c = context(&p.hook("subagent-start", &ev), "SubagentStart");
    assert!(c.contains(&want), "{c}");
    assert_eq!(t19_children(&p, "t19-par").len(), 2);
}
