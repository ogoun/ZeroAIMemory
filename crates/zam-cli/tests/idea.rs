//! `zam idea` против настоящего zam-server в том же процессе тестов (M5.7 §7.3): создание
//! (`origin` человека по умолчанию, под агентом — обязателен), лента с фильтрами, предложение,
//! «начать» (повтор — код 4), отсрочка (`--until`, плохая дата — код 6), отказ, формы id,
//! `zam read w:<id>` и регрессия `zam question answer --by`. Сервер без моделей.
//!
//! M6.5 (задача 6): `zam idea review` (исходы, повтор по `--key`, флаги — код 6 до сети, старый
//! сервер — код 1), `zam idea queue` и статус `returned` в ленте.
mod common;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use zam_server::build;
use zam_server::config::{IdeasMode, ServerConfig};
use zam_store::Access;

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e52";

fn tmp(name: &str) -> PathBuf {
    common::sandbox("cli-idea", name)
}

/// Сервер в фоновом потоке с режимом идей `mode` (`[ideas] default_mode`): адрес и токен `rw`
/// пользователя «Анна».
fn start(name: &str, mode: IdeasMode) -> (String, String) {
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
            cfg.ideas.default_mode = mode;
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
    rx.recv().unwrap()
}

/// Окружение теста: сервер, токен, каталог проекта.
struct T {
    url: String,
    tok: String,
    dir: PathBuf,
}

impl T {
    /// Режим идей по умолчанию (`review`, M6.5).
    fn new(name: &str) -> T {
        T::with_mode(name, IdeasMode::default())
    }

    /// Режим идей M5.7 `propose`: в режиме `review` `propose` новой идеи — 409 (M6.5 §7).
    fn propose_mode(name: &str) -> T {
        T::with_mode(name, IdeasMode::Propose)
    }

    fn with_mode(name: &str, mode: IdeasMode) -> T {
        let (url, tok) = start(name, mode);
        T {
            url,
            tok,
            dir: repo(&format!("{name}-repo")),
        }
    }

    fn cmd(&self, args: &[&str]) -> assert_cmd::Command {
        let mut c = common::zam_cmd();
        c.env("ZAM_URL", &self.url)
            .env("ZAM_TOKEN", &self.tok)
            .env("HOME", &self.dir)
            .env("USERPROFILE", &self.dir)
            .current_dir(&self.dir)
            .args(args)
            .write_stdin(Vec::new());
        c
    }

    /// Человек в терминале: окружение без переменных агента.
    fn z(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    /// Команду запустил агент (`ZAM_AGENT=1`).
    fn agent(&self, args: &[&str]) -> Output {
        self.cmd(args)
            .env("ZAM_AGENT", "1")
            .env("ZAM_SESSION", "-")
            .output()
            .unwrap()
    }

    /// Новая идея человека: голый id.
    fn idea(&self, title: &str, extra: &[&str]) -> String {
        let mut args = vec!["idea", "new", title];
        args.extend_from_slice(extra);
        let o = ok(&self.z(&args));
        o.trim().split_once('@').unwrap().0.to_string()
    }

    fn json(&self, args: &[&str]) -> Value {
        let mut a = args.to_vec();
        a.push("--json");
        serde_json::from_str(&ok(&self.z(&a))).unwrap()
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

/// Каталог проекта с `solution.toml`.
fn repo(name: &str) -> PathBuf {
    let r = tmp(name);
    std::fs::create_dir_all(r.join(".zeroaimemory")).unwrap();
    std::fs::write(
        r.join(".zeroaimemory").join("solution.toml"),
        format!("agent_solution_id = \"{G}\"\nname = \"Идеи\"\n"),
    )
    .unwrap();
    r
}

fn entry(t: &T, id: &str) -> Value {
    // `zam read e:<id> --json` — элемент пачки `/read`; для полей записи — лента
    let v = t.json(&[
        "idea",
        "list",
        "--status",
        "new,returned,proposed,accepted,deferred,declined,done",
    ]);
    let v2 = t.json(&[
        "idea",
        "list",
        "--knowledge",
        "--status",
        "new,returned,proposed,accepted,deferred,declined,done",
    ]);
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .chain(v2["items"].as_array().unwrap())
        .find(|e| e["id"] == id)
        .cloned()
        .unwrap_or_else(|| panic!("нет идеи {id}"))
}

#[test]
fn idea_new_list_human_default() {
    let t = T::new("human");
    let id = t.idea(
        "Тёмная тема",
        &[
            "--body",
            "Замысел: тема по системной",
            "--priority",
            "high",
            "--tag",
            "ui",
            "--tag",
            "тема",
            "--horizon",
            "2099-01-01",
        ],
    );
    let e = entry(&t, &id);
    assert_eq!(e["data"]["origin"], "human");
    assert_eq!(e["data"]["priority"], "high");
    assert_eq!(e["data"]["horizon"], "2099-01-01");
    assert_eq!(e["body"], "Замысел: тема по системной");
    assert_eq!(e["tags"], serde_json::json!(["ui", "тема"]));
    let l = ok(&t.z(&["idea", "list"]));
    assert!(
        l.contains(&format!("w:{id} [new] [high] Тёмная тема")),
        "{l}"
    );
    assert!(!l.contains("(от агента)"), "{l}");

    // тело из файла; --body и --file вместе — ошибка аргументов (код 6)
    let f = t.dir.join("idea.md");
    std::fs::write(&f, "# План\nиз файла").unwrap();
    let id2 = t.idea("Из файла", &["--file", f.to_str().unwrap()]);
    assert_eq!(entry(&t, &id2)["body"], "# План\nиз файла");
    let o = t.z(&["idea", "new", "x", "--body", "a", "--file", "idea.md"]);
    assert_eq!(code(&o), Some(6), "{}", err(&o));
}

#[test]
fn idea_new_under_agent_requires_origin() {
    let t = T::new("agent-origin");
    let o = t.agent(&["idea", "new", "Кэш"]);
    assert_eq!(code(&o), Some(6), "{}", err(&o));
    assert!(
        err(&o).contains("--origin human") && err(&o).contains("agent"),
        "{}",
        err(&o)
    );
    let o = ok(&t.agent(&["idea", "new", "Кэш", "--origin", "agent"]));
    let id = o.trim().split_once('@').unwrap().0.to_string();
    assert_eq!(entry(&t, &id)["data"]["origin"], "agent");
    // отказ без --origin ничего не создал: идея «Кэш» одна
    let j = t.json(&["idea", "list"]);
    assert_eq!(j["items"].as_array().unwrap().len(), 1, "{j}");
    let l = ok(&t.z(&["idea", "list"]));
    assert!(
        l.contains(&format!("w:{id} [new] [normal] Кэш (от агента)")),
        "{l}"
    );
    // агент записывает идею человека с его слов
    let o = ok(&t.agent(&["idea", "new", "Поиск", "--origin", "human"]));
    let id = o.trim().split_once('@').unwrap().0.to_string();
    assert_eq!(entry(&t, &id)["data"]["origin"], "human");
}

#[test]
fn idea_propose_prints_question_ref() {
    let t = T::propose_mode("propose");
    let id = t.idea("Экспорт в PDF", &[]);
    let p = ok(&t.z(&[
        "idea",
        "propose",
        &format!("w:{id}"),
        "--text",
        "сейчас удобно",
    ]));
    assert!(
        p.starts_with(&format!("Предложено w:{id} → вопрос q:")),
        "{p}"
    );
    let qid = p.trim().rsplit("q:").next().unwrap().to_string();
    // вопрос-предложение виден в ленте вопросов с вариантами сервера
    let q = ok(&t.z(&["question", "list"]));
    assert!(q.contains(&qid), "{q}");
    assert!(q.contains("accept=начать"), "{q}");
    // повтор при открытом вопросе — тот же вопрос
    let p2 = ok(&t.z(&["idea", "propose", &id]));
    assert!(p2.contains(&format!("q:{qid}")), "{p2}");
    let l = ok(&t.z(&["idea", "list"]));
    assert!(l.contains(&format!("w:{id} [proposed]")), "{l}");
    // --json — ответ сервера
    let j = t.json(&["idea", "propose", &id]);
    assert_eq!(j["question"]["id"], qid.as_str());
    // идея агента не предлагается — 409, код 4
    let o = ok(&t.agent(&["idea", "new", "Своя", "--origin", "agent"]));
    let aid = o.trim().split_once('@').unwrap().0.to_string();
    let o = t.z(&["idea", "propose", &aid]);
    assert_eq!(code(&o), Some(4), "{}", err(&o));
}

#[test]
fn idea_accept_prints_task_and_twice_code_4() {
    let t = T::new("accept");
    let id = t.idea("Импорт Obsidian", &["--body", "из хранилища"]);
    let a = ok(&t.z(&["idea", "accept", &format!("w:{id}")]));
    assert!(
        a.starts_with(&format!("Идея w:{id}: accepted → задача k:")),
        "{a}"
    );
    let tid = a.trim().rsplit("k:").next().unwrap().to_string();
    let tasks = ok(&t.z(&["task", "list"]));
    assert!(
        tasks.contains(&format!("{tid} [open] «Импорт Obsidian»")),
        "{tasks}"
    );
    let l = ok(&t.z(&["idea", "list"]));
    assert!(
        l.contains(&format!(
            "w:{id} [accepted] [normal] Импорт Obsidian → k:{tid}"
        )),
        "{l}"
    );
    // повтор — 409 already-decided: код 4 с той же задачей
    let o = t.z(&["idea", "accept", &id, "--by", "human", "--channel", "chat"]);
    assert_eq!(code(&o), Some(4), "{}", err(&o));
    assert!(err(&o).contains(&format!("k:{tid}")), "{}", err(&o));
    // --json: `task` в ответе
    let id2 = t.idea("Вторая", &[]);
    let j = t.json(&["idea", "accept", &id2]);
    assert_eq!(j["entry"]["status"], "accepted");
    assert_eq!(j["task"]["kind"], "task");
    assert_eq!(j["entry"]["data"]["decided"]["channel"], "cli");
}

#[test]
fn idea_accept_under_agent_requires_by() {
    let t = T::new("accept-agent");
    let id = t.idea("Горячие клавиши", &[]);
    let o = t.agent(&["idea", "accept", &id]);
    assert_eq!(code(&o), Some(6), "{}", err(&o));
    assert!(
        err(&o).contains("--by human") && err(&o).contains("agent"),
        "{}",
        err(&o)
    );
    assert!(ok(&t.z(&["idea", "list"])).contains(&format!("w:{id} [new]")));
    // с --by human (слова человека) — принято
    let a = ok(&t.agent(&["idea", "accept", &id, "--by", "human", "--channel", "chat"]));
    assert!(a.contains("accepted → задача k:"), "{a}");
    for cmd in ["defer", "decline"] {
        let id = t.idea(&format!("Под агентом {cmd}"), &[]);
        let o = t.agent(&["idea", cmd, &id]);
        assert_eq!(code(&o), Some(6), "{cmd}: {}", err(&o));
    }
}

#[test]
fn idea_accept_by_agent_code_3() {
    let t = T::new("by-agent");
    let id = t.idea("Синхронизация", &[]);
    let o = t.z(&["idea", "accept", &id, "--by", "agent"]);
    assert_eq!(code(&o), Some(3), "{}", err(&o));
    assert!(err(&o).contains("by: human"), "{}", err(&o));
    assert!(ok(&t.z(&["idea", "list"])).contains(&format!("w:{id} [new]")));
    // отказ от идеи человека агентом — тоже 403
    let o = t.z(&["idea", "decline", &id, "--by", "agent"]);
    assert_eq!(code(&o), Some(3), "{}", err(&o));
    // свою идею агент снимает сам
    let o = ok(&t.agent(&["idea", "new", "Своя", "--origin", "agent"]));
    let aid = o.trim().split_once('@').unwrap().0.to_string();
    let d = ok(&t.agent(&[
        "idea",
        "decline",
        &aid,
        "--by",
        "agent",
        "--reason",
        "лишнее",
    ]));
    assert!(d.contains(&format!("Идея w:{aid}: declined")), "{d}");
}

#[test]
fn idea_defer_until_and_bad_date_code_6() {
    let t = T::new("defer");
    let id = t.idea("Мобильная версия", &[]);
    let d = ok(&t.z(&["idea", "defer", &id, "--until", "2099-03-01"]));
    assert_eq!(d.trim(), format!("Идея w:{id}: deferred до 2099-03-01"));
    // повторный defer — новый horizon
    ok(&t.z(&["idea", "defer", &id, "--until", "2099-04-01"]));
    assert_eq!(entry(&t, &id)["data"]["horizon"], "2099-04-01");
    // без --until — сервер ставит сегодня + defer_days
    let d = ok(&t.z(&["idea", "defer", &id]));
    assert!(d.contains("deferred до 20"), "{d}");
    // неверный формат — код 6 в CLI, до сервера
    for bad in [
        "2099-3-1",
        "01.03.2099",
        "2099-02-30",
        "2099-13-01",
        "2099-03-01T00:00",
        "завтра",
    ] {
        let o = t.z(&["idea", "defer", &id, "--until", bad]);
        assert_eq!(code(&o), Some(6), "{bad}: {}", err(&o));
        assert!(err(&o).contains("--until"), "{bad}: {}", err(&o));
    }
    let o = t.z(&["idea", "new", "x", "--horizon", "2099-02-30"]);
    assert_eq!(code(&o), Some(6), "{}", err(&o));
    assert!(err(&o).contains("--horizon"), "{}", err(&o));

    // «позже сегодняшней даты UTC» решает только сервер: сегодня и прошедшая — 422 → код 6
    let today = zam_store::time::utc_today().to_string();
    for day in [today.as_str(), "2000-01-01"] {
        let o = t.z(&["idea", "defer", &id, "--until", day]);
        assert_eq!(code(&o), Some(6), "{day}: {}", err(&o));
        assert!(
            err(&o).contains("не раньше завтрашней даты (UTC") && err(&o).contains("HTTP 422"),
            "{day}: {}",
            err(&o)
        );
        let o = t.z(&["idea", "new", "x", "--horizon", day]);
        assert_eq!(code(&o), Some(6), "{day}: {}", err(&o));
        assert!(err(&o).contains("HTTP 422"), "{day}: {}", err(&o));
    }
    // идея не тронута: horizon прежний
    assert_eq!(entry(&t, &id)["status"], "deferred");
}

/// 422 сервера → код 6: `--space` — GUID решения, которого нет.
#[test]
fn idea_server_422_code_6() {
    let t = T::new("server-422");
    let id = t.idea("Задача в чужом решении", &[]);
    let o = t.z(&[
        "idea",
        "accept",
        &id,
        "--space",
        "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9eff",
    ]);
    assert_eq!(code(&o), Some(6), "{}{}", out(&o), err(&o));
    assert!(err(&o).contains("HTTP 422"), "{}", err(&o));
    assert!(ok(&t.z(&["idea", "list"])).contains(&format!("w:{id} [new]")));
}

#[test]
fn idea_decline_reason() {
    let t = T::new("decline");
    let id = t.idea("Плагины", &[]);
    let d = ok(&t.z(&["idea", "decline", &id, "--reason", "не сейчас и не так"]));
    assert_eq!(d.trim(), format!("Идея w:{id}: declined"));
    let e = entry(&t, &id);
    assert_eq!(e["status"], "declined");
    assert_eq!(e["data"]["decided"]["decision"], "decline");
    assert_eq!(e["data"]["decided"]["by"], "human");
    // повторный отказ — 409 transition-not-allowed, код 4
    let o = t.z(&["idea", "decline", &id]);
    assert_eq!(code(&o), Some(4), "{}", err(&o));
    // по умолчанию в ленте отклонённых нет, по --status — есть
    assert!(!ok(&t.z(&["idea", "list"])).contains(&id));
    let l = ok(&t.z(&["idea", "list", "--status", "declined"]));
    assert!(l.contains(&format!("w:{id} [declined]")), "{l}");
}

#[test]
fn idea_list_filters_status_origin_knowledge() {
    let t = T::new("filters");
    let h = t.idea("Человека", &[]);
    let o = ok(&t.agent(&["idea", "new", "Агента", "--origin", "agent"]));
    let a = o.trim().split_once('@').unwrap().0.to_string();
    let d = t.idea("Отложенная", &[]);
    ok(&t.z(&["idea", "defer", &d, "--until", "2099-05-05"]));
    let k = t.idea("Общая", &["--knowledge"]);

    let all = ok(&t.z(&["idea", "list"]));
    assert!(
        all.contains(&h) && all.contains(&a) && all.contains(&d),
        "{all}"
    );
    assert!(!all.contains(&k), "Knowledge — только с --knowledge: {all}");

    let hum = ok(&t.z(&["idea", "list", "--origin", "human"]));
    assert!(hum.contains(&h) && !hum.contains(&a), "{hum}");
    let ag = ok(&t.z(&["idea", "list", "--origin", "agent"]));
    assert!(ag.contains(&a) && !ag.contains(&h), "{ag}");

    // --status повторяемый и через запятую
    let st = ok(&t.z(&["idea", "list", "--status", "deferred"]));
    assert!(st.contains(&d) && !st.contains(&h), "{st}");
    let st = ok(&t.z(&["idea", "list", "--status", "deferred", "--status", "new"]));
    assert!(st.contains(&d) && st.contains(&h), "{st}");
    let st = ok(&t.z(&["idea", "list", "--status", "deferred,new"]));
    assert!(st.contains(&d) && st.contains(&h), "{st}");
    let o = t.z(&["idea", "list", "--status", "open"]);
    assert_eq!(code(&o), Some(6), "{}", err(&o));

    let kn = ok(&t.z(&["idea", "list", "--knowledge"]));
    assert!(kn.contains(&format!("w:{k} [new] [normal] Общая")), "{kn}");
    assert!(!kn.contains(&h), "{kn}");

    let j = t.json(&["idea", "list", "--origin", "agent"]);
    assert_eq!(j["items"].as_array().unwrap().len(), 1);
    assert_eq!(j["items"][0]["id"], a.as_str());
}

#[test]
fn idea_id_forms() {
    let t = T::new("ids");
    let a = t.idea("Первая", &[]);
    let b = t.idea("Вторая", &[]);
    let c = t.idea("Третья", &[]);
    ok(&t.z(&["idea", "decline", &format!("w:{a}")]));
    ok(&t.z(&["idea", "decline", &format!("e:{b}")]));
    ok(&t.z(&["idea", "decline", &c]));
    let l = ok(&t.z(&["idea", "list", "--status", "declined"]));
    for id in [&a, &b, &c] {
        assert!(l.contains(id.as_str()), "{l}");
    }
    // мусор и чужие буквы — код 6 без запроса к серверу
    for bad in [
        "мусор".to_string(),
        format!("k:{a}"),
        format!("q:{a}"),
        "w:".to_string(),
        format!("w:{}", a.to_lowercase()),
        "w:01ABC".to_string(),
    ] {
        let o = t.z(&["idea", "accept", &bad]);
        assert_eq!(code(&o), Some(6), "{bad}: {}", err(&o));
        assert!(err(&o).contains("w:<id>"), "{bad}: {}", err(&o));
    }
}

#[test]
fn read_w_ref_prints_body() {
    let t = T::new("read");
    let id = t.idea("Читаемая", &["--body", "Тело идеи для чтения"]);
    let r = ok(&t.z(&["read", &format!("w:{id}")]));
    assert!(r.contains("Тело идеи для чтения"), "{r}");
    let r = ok(&t.z(&["read", &format!("e:{id}")]));
    assert!(r.contains("Тело идеи для чтения"), "{r}");
}

/// Обобщение `resolve_by` до `resolve_human_agent`: `zam question answer` прежний — агенту без
/// `--by` код 6 с прежней подсказкой, человеку — `human` по умолчанию.
#[test]
fn question_answer_by_unchanged() {
    let t = T::new("question-by");
    let ask = |title: &str| {
        let q = ok(&t.z(&["question", "ask", "--title", title]));
        q.trim().split_once('@').unwrap().0.to_string()
    };
    let q1 = ask("Брать ли кэш?");
    let o = t.agent(&["question", "answer", &q1, "--text", "да"]);
    assert_eq!(code(&o), Some(6), "{}", err(&o));
    assert!(
        err(&o).contains("укажите --by human — ответ человека, --by agent — решение агента"),
        "{}",
        err(&o)
    );
    let j = t.json(&["question", "answer", &q1, "--text", "да"]);
    assert_eq!(j["entry"]["data"]["answer"]["by"], "human");
    assert_eq!(j["entry"]["data"]["answer"]["channel"], "cli");
}

/// Задача 7 (§16): `zam idea propose` по идее со снятой задачей — без `--text` код 6 (422),
/// с `--text` — «Предложено вернуть w:<id> → вопрос q:<id>», варианты «вернуть / отказаться».
#[test]
fn idea_propose_reopen_prints() {
    let t = T::new("propose-reopen");
    let id = t.idea("Кэш выжимок", &[]);
    let a = ok(&t.z(&["idea", "accept", &format!("w:{id}")]));
    let tid = a.trim().rsplit("k:").next().unwrap().to_string();
    ok(&t.z(&["task", "status", &format!("k:{tid}"), "dropped"]));
    let o = t.z(&["idea", "propose", &format!("w:{id}")]);
    assert_eq!(code(&o), Some(6), "{}", err(&o));
    assert!(err(&o).contains("почему идею пора вернуть"), "{}", err(&o));
    let p = ok(&t.z(&[
        "idea",
        "propose",
        &format!("w:{id}"),
        "--text",
        "кэш снова тормозит",
    ]));
    assert!(
        p.starts_with(&format!("Предложено вернуть w:{id} → вопрос q:")),
        "{p}"
    );
    let qid = p.trim().rsplit("q:").next().unwrap().to_string();
    let q = ok(&t.z(&["question", "list"]));
    assert!(q.contains(&qid), "{q}");
    assert!(q.contains("reopen=вернуть"), "{q}");
    // идея остаётся принятой
    let l = ok(&t.z(&["idea", "list"]));
    assert!(l.contains(&format!("w:{id} [accepted]")), "{l}");
}

/// Сообщение `zam idea review|queue` у сервера без флага `idea_review` (M6.5 §10).
const OLD_SERVER_REVIEW: &str =
    "сервер не поддерживает разбор идей: нужен zam-server 0.0.12-m6.5 или новее";

/// Разбор для человека не короче `ideas.review_min_chars` сервера (200 символов).
fn summary() -> String {
    let s = "Что просит идея: выгрузку заметок решения в PDF для передачи заказчику. Что уже \
             есть: экспорт пространства в архив .zip (zam export), задачи плана о печати нет, \
             ТЗ о форматах выгрузки молчит. Решение и почему: в план — отдельная задача с \
             шагами в теле, похожих отклонённых идей нет."
        .to_string();
    assert!(s.chars().count() >= 200, "{}", s.chars().count());
    s
}

impl T {
    /// Файл `name` в каталоге проекта: путь строкой для аргумента.
    fn file(&self, name: &str, text: &str) -> String {
        let p = self.dir.join(name);
        std::fs::write(&p, text).unwrap();
        p.to_str().unwrap().to_string()
    }
}

/// `zam` человека против заглушки `url` из каталога проекта `dir`.
fn stub_z(url: &str, dir: &Path, args: &[&str]) -> Output {
    common::zam_cmd()
        .env("ZAM_URL", url)
        .env("ZAM_TOKEN", "t")
        .env("HOME", dir)
        .env("USERPROFILE", dir)
        .current_dir(dir)
        .args(args)
        .write_stdin(Vec::new())
        .output()
        .unwrap()
}

/// `zam idea review --plan` (M6.5 §5): новая задача с планом шагов и приоритетом —
/// «Разобрано w:<id>: plan → задача k:<id>», повтор с тем же `--key` — та же задача
/// « (повтор)»; под агентом `--by` не нужен. `--plan --task k:<id>` привязывает к открытой
/// задаче, `--return` — статус `returned` с «не хватает» в `zam idea list --status returned`
/// (и в ленте по умолчанию), `--json` — ответ сервера, повторный разбор — код 4.
#[test]
fn idea_review_plan_prints_task() {
    let t = T::new("review-plan");
    let id = t.idea(
        "Экспорт в PDF",
        &["--body", "Выгрузить заметки решения в PDF"],
    );
    let s = t.file("s.md", &summary());
    let plan = t.file("plan.md", "1. Шаблон страницы\n2. Рендер Markdown в PDF");
    let w = format!("w:{id}");
    let args = [
        "idea",
        "review",
        w.as_str(),
        "--plan",
        "--summary-file",
        s.as_str(),
        "--task-body-file",
        plan.as_str(),
        "--task-priority",
        "high",
        "--key",
        "review-pdf-1",
    ];
    let r = ok(&t.agent(&args));
    assert!(
        r.starts_with(&format!("Разобрано w:{id}: plan → задача k:")),
        "{r}"
    );
    let tid = r.trim().rsplit("k:").next().unwrap().to_string();
    // повтор после «обрыва связи» с тем же ключом: та же задача, второй нет
    let r2 = ok(&t.agent(&args));
    assert_eq!(
        r2.trim(),
        format!("Разобрано w:{id}: plan → задача k:{tid} (повтор)")
    );
    let tasks = ok(&t.z(&["task", "list"]));
    assert_eq!(tasks.matches("«Экспорт в PDF»").count(), 1, "{tasks}");
    let task: Value =
        serde_json::from_str(&ok(&t.z(&["task", "show", &format!("k:{tid}"), "--json"]))).unwrap();
    assert_eq!(task["data"]["priority"], "high", "{task}");
    let body = task["body"].as_str().unwrap();
    assert!(
        body.contains("2. Рендер Markdown в PDF") && body.contains(&format!("w:{id}")),
        "{body}"
    );
    let l = ok(&t.z(&["idea", "list"]));
    assert!(
        l.contains(&format!(
            "w:{id} [accepted] [normal] Экспорт в PDF → k:{tid}"
        )),
        "{l}"
    );
    let e = entry(&t, &id);
    assert_eq!(e["data"]["review"]["outcome"], "plan", "{e}");
    assert_eq!(e["data"]["review"]["summary"], summary().as_str());

    // уже открытая задача плана покрывает вторую идею (человек в терминале — тоже без --by)
    let id2 = t.idea("PDF для заказчика", &[]);
    let r = ok(&t.z(&[
        "idea",
        "review",
        &id2,
        "--plan",
        "--task",
        &format!("k:{tid}"),
        "--summary-file",
        &s,
        "--similar",
        &format!("w:{id}"),
    ]));
    assert_eq!(
        r.trim(),
        format!("Разобрано w:{id2}: plan → задача k:{tid}")
    );

    // на доработку: статус returned, в ленте — что дописать
    let id3 = t.idea("Тёмная тема", &[]);
    let r = ok(&t.agent(&[
        "idea",
        "review",
        &id3,
        "--return",
        "--missing",
        "Какие экраны?\nНужна ли системная тема?",
        "--summary-file",
        &s,
    ]));
    assert_eq!(r.trim(), format!("Разобрано w:{id3}: return"));
    let l = ok(&t.z(&["idea", "list", "--status", "returned"]));
    assert!(
        l.contains(&format!(
            "w:{id3} [returned] [normal] Тёмная тема — не хватает: Какие экраны?"
        )),
        "{l}"
    );
    assert!(!l.contains(&id) && !l.contains("системная"), "{l}");
    let l = ok(&t.z(&["idea", "list"]));
    assert!(l.contains(&format!("w:{id3} [returned]")), "{l}");
    assert_eq!(entry(&t, &id3)["status"], "returned");

    // --json — ответ сервера; повторный разбор разобранной — 409, код 4
    let id4 = t.idea("Голосовой ввод", &[]);
    let j = t.json(&["idea", "review", &id4, "--decline", "--summary-file", &s]);
    assert_eq!(j["entry"]["status"], "declined", "{j}");
    assert_eq!(j["replay"], false, "{j}");
    assert!(j.get("task").is_none(), "{j}");
    let o = t.agent(&["idea", "review", &id4, "--decline", "--summary-file", &s]);
    assert_eq!(code(&o), Some(4), "{}", err(&o));

    // 422 сервера (короткий разбор, исход done без ссылки на сделанное) — код 6
    let short = t.file("short.md", "мало");
    let id5 = t.idea("Короткий разбор", &[]);
    let o = t.agent(&[
        "idea",
        "review",
        &id5,
        "--decline",
        "--summary-file",
        &short,
    ]);
    assert_eq!(code(&o), Some(6), "{}", err(&o));
    assert!(
        err(&o).contains("summary") && err(&o).contains("HTTP 422"),
        "{}",
        err(&o)
    );
    let o = t.agent(&["idea", "review", &id5, "--done", "--summary-file", &s]);
    assert_eq!(code(&o), Some(6), "{}", err(&o));
    assert!(err(&o).contains("basis"), "{}", err(&o));
    assert_eq!(entry(&t, &id5)["status"], "new");
    // нет такой идеи — код 5, не «старый сервер»
    let ghost = zam_core::Id::new().to_string();
    let o = t.agent(&["idea", "review", &ghost, "--decline", "--summary-file", &s]);
    assert_eq!(code(&o), Some(5), "{}", err(&o));
    assert!(!err(&o).contains("не поддерживает"), "{}", err(&o));
}

/// Флаги `zam idea review` (M6.5 §5) и `zam idea queue --limit` проверяются до сети — код 6:
/// исход обязателен и один, `--return` — с `--missing`, `--missing` — только у `--return`, поля
/// задачи — только у `--plan`, `--task` — без полей новой задачи; ссылки и файлы.
#[test]
fn idea_review_flags_exclusive_and_missing() {
    let (url, seen) = common::stub(500, "{}", Duration::ZERO);
    let dir = repo("review-flags-repo");
    std::fs::write(dir.join("s.md"), summary()).unwrap();
    let id = zam_core::Id::new().to_string();
    let w = format!("w:{id}");
    let k = format!("k:{}", zam_core::Id::new());
    let review = |extra: &[&str]| {
        let mut a = vec!["idea", "review", w.as_str(), "--summary-file", "s.md"];
        a.extend_from_slice(extra);
        stub_z(&url, &dir, &a)
    };
    let cases: Vec<(&str, Output)> = vec![
        ("без исхода", review(&[])),
        ("два исхода", review(&["--plan", "--decline"])),
        (
            "return и done",
            review(&["--return", "--missing", "x", "--done"]),
        ),
        ("return без missing", review(&["--return"])),
        ("missing у plan", review(&["--plan", "--missing", "x"])),
        ("task у decline", review(&["--decline", "--task", &k])),
        (
            "task-title у return",
            review(&["--return", "--missing", "x", "--task-title", "T"]),
        ),
        (
            "task и task-title",
            review(&["--plan", "--task", &k, "--task-title", "T"]),
        ),
        (
            "task и task-body-file",
            review(&["--plan", "--task", &k, "--task-body-file", "s.md"]),
        ),
        (
            "task и task-priority",
            review(&["--plan", "--task", &k, "--task-priority", "high"]),
        ),
        (
            "плохой приоритет",
            review(&["--plan", "--task-priority", "urgent"]),
        ),
        ("плохая задача", review(&["--plan", "--task", "мусор"])),
        ("чужая буква задачи", review(&["--plan", "--task", &w])),
        (
            "нет файла разбора",
            stub_z(
                &url,
                &dir,
                &[
                    "idea",
                    "review",
                    &w,
                    "--decline",
                    "--summary-file",
                    "нет.md",
                ],
            ),
        ),
        (
            "нет файла плана",
            review(&["--plan", "--task-body-file", "нет.md"]),
        ),
        (
            "без --summary-file",
            stub_z(&url, &dir, &["idea", "review", &w, "--decline"]),
        ),
        (
            "плохая идея",
            stub_z(
                &url,
                &dir,
                &["idea", "review", &k, "--decline", "--summary-file", "s.md"],
            ),
        ),
        (
            "queue --limit 0",
            stub_z(&url, &dir, &["idea", "queue", "--limit", "0"]),
        ),
        (
            "queue --limit 21",
            stub_z(&url, &dir, &["idea", "queue", "--limit", "21"]),
        ),
    ];
    for (what, o) in &cases {
        assert_eq!(code(o), Some(6), "{what}: {}{}", out(o), err(o));
    }
    // понятные подсказки там, где их даёт сам `zam`
    let msg = |what: &str| err(&cases.iter().find(|(w, _)| *w == what).unwrap().1);
    assert!(
        msg("плохая задача").contains("k:<id>"),
        "{}",
        msg("плохая задача")
    );
    assert!(
        msg("плохая идея").contains("w:<id>"),
        "{}",
        msg("плохая идея")
    );
    assert!(
        msg("нет файла разбора").contains("нет.md"),
        "{}",
        msg("нет файла разбора")
    );
    assert!(
        seen.lock().unwrap().is_empty(),
        "{:?}",
        seen.lock().unwrap()
    );
}

/// `zam idea queue --limit N` (M6.5 §7): пакет `idea_review` — N идей очереди с шагами разбора и
/// строкой «показано k из N»; `--json` — ответ сервера (`items` с `reason: idea_review`); пустая
/// очередь — строка-уведомление, код 0.
#[test]
fn idea_queue_lists_new() {
    let t = T::new("queue");
    // решение зарегистрировано первой идеей; решённая человеком в очередь не входит
    let first = t.idea("Решённая", &[]);
    ok(&t.z(&["idea", "decline", &first]));
    let empty = ok(&t.z(&["idea", "queue"]));
    assert_eq!(empty.trim(), "Идей на разбор нет.", "{empty}");
    let ids: Vec<String> = ["Экспорт в PDF", "Тёмная тема", "Горячие клавиши"]
        .iter()
        .map(|title| t.idea(title, &[]))
        .collect();
    let q = ok(&t.agent(&["idea", "queue", "--limit", "2"]));
    assert!(q.contains("Показано идей на разбор: 2 из 3."), "{q}");
    assert!(q.contains("Шаги разбора:"), "{q}");
    assert!(q.contains("action=review"), "{q}");
    let j = t.json(&["idea", "queue", "--limit", "2"]);
    let items = j["items"].as_array().unwrap();
    let shown: Vec<&str> = items
        .iter()
        .filter(|i| i["reason"] == "idea_review")
        .filter_map(|i| i["id"].as_str())
        .collect();
    assert_eq!(shown.len(), 2, "{j}");
    assert!(
        shown
            .iter()
            .all(|s| ids.iter().any(|i| *s == format!("w:{i}"))),
        "{j}"
    );
    // без --limit — вся очередь (по умолчанию 5)
    let q = ok(&t.z(&["idea", "queue"]));
    assert!(q.contains("Показано идей на разбор: 3 из 3."), "{q}");
    for title in ["Экспорт в PDF", "Тёмная тема", "Горячие клавиши"]
    {
        assert!(q.contains(title), "{title}: {q}");
    }
}

/// Старый сервер (M6.5 §10): 404 на `/review`, а в `GET /health` `features` нет `idea_review`
/// (или нет самого поля, сервер до M6) — код 1 и [`OLD_SERVER_REVIEW`]; `zam idea queue` — так же
/// по 400 на событие `idea_review`. Сервер с флагом и 404 — нет такой идеи, код 5.
#[test]
fn idea_review_old_server_message() {
    let dir = repo("review-old-repo");
    std::fs::write(dir.join("s.md"), summary()).unwrap();
    let id = zam_core::Id::new().to_string();
    let review = [
        "idea",
        "review",
        id.as_str(),
        "--decline",
        "--summary-file",
        "s.md",
    ];
    let not_found =
        r#"{"type":"about:blank","title":"Not Found","status":404,"detail":"ресурс не найден"}"#;
    let stub = |health: &'static str| {
        common::stub_with(Arc::new(move |_: &str, p: &str, _: usize| {
            if p == "/api/v1/health" {
                (200, health, Duration::ZERO)
            } else if p == "/api/v1/recall" {
                (
                    400,
                    r#"{"type":"about:blank","title":"Bad Request","status":400,"detail":"некорректное тело: unknown variant `idea_review`"}"#,
                    Duration::ZERO,
                )
            } else {
                (404, not_found, Duration::ZERO)
            }
        }))
    };
    for health in [
        r#"{"status":"ok","version":"0.0.11-m6","features":["m6","export","import"]}"#,
        r#"{"status":"ok","version":"0.0.10-m5.7"}"#,
    ] {
        let (url, seen) = stub(health);
        let o = stub_z(&url, &dir, &review);
        assert_eq!(code(&o), Some(1), "{health}: {}", err(&o));
        assert_eq!(err(&o).trim(), OLD_SERVER_REVIEW, "{health}");
        let s = seen.lock().unwrap().clone();
        assert_eq!(s.len(), 2, "{s:?}");
        assert_eq!(
            (s[0].0.as_str(), s[0].1.as_str()),
            ("POST", format!("/api/v1/entries/{id}/review").as_str())
        );
        assert_eq!(
            (s[1].0.as_str(), s[1].1.as_str()),
            ("GET", "/api/v1/health")
        );
        let o = stub_z(&url, &dir, &["idea", "queue"]);
        assert_eq!(code(&o), Some(1), "{health}: {}", err(&o));
        assert_eq!(err(&o).trim(), OLD_SERVER_REVIEW, "{health}");
    }
    // новый сервер: 404 — нет такой идеи (код 5), 400 пакета — неверный ввод (код 6)
    let (url, _) =
        stub(r#"{"status":"ok","version":"0.0.12-m6.5","features":["m6","idea_review"]}"#);
    let o = stub_z(&url, &dir, &review);
    assert_eq!(code(&o), Some(5), "{}", err(&o));
    assert!(err(&o).contains("ресурс не найден"), "{}", err(&o));
    let o = stub_z(&url, &dir, &["idea", "queue"]);
    assert_eq!(code(&o), Some(6), "{}", err(&o));
    assert!(!err(&o).contains("не поддерживает"), "{}", err(&o));
}
