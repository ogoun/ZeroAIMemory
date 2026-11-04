//! `zam idea` против настоящего zam-server в том же процессе тестов (M5.7 §7.3): создание
//! (`origin` человека по умолчанию, под агентом — обязателен), лента с фильтрами, предложение,
//! «начать» (повтор — код 4), отсрочка (`--until`, плохая дата — код 6), отказ, формы id,
//! `zam read w:<id>` и регрессия `zam question answer --by`. Сервер без моделей.
mod common;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Output;

use serde_json::Value;
use zam_server::build;
use zam_server::config::ServerConfig;
use zam_store::Access;

const G: &str = "0192f1c4-7a1e-7c3b-9a55-2b1f6f0d9e52";

fn tmp(name: &str) -> PathBuf {
    common::sandbox("cli-idea", name)
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
            let cfg = ServerConfig {
                data_dir: data,
                ..ServerConfig::default()
            };
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
    fn new(name: &str) -> T {
        let (url, tok) = start(name);
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
        "new,proposed,accepted,deferred,declined,done",
    ]);
    let v2 = t.json(&[
        "idea",
        "list",
        "--knowledge",
        "--status",
        "new,proposed,accepted,deferred,declined,done",
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
    let t = T::new("propose");
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
